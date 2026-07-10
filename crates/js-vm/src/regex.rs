//! A hybrid regex engine: pattern → AST → (Pike VM | backtracking simulator).
//!
//! The engine is split into two execution strategies over one shared AST:
//!
//! - **Pike VM** (Thompson NFA, after Russ Cox / RE2): linear-time `O(n·k)`
//!   simulation with submatch tracking, *no backtracking*. Used for the common
//!   feature subset (literals, `.`, classes, anchors, `*+?{}`, groups, `|`).
//!
//! - **Backtracking simulator**: a continuation-passing recursive matcher over
//!   the AST. Used only when the pattern needs features that a pure NFA cannot
//!   express — **lookaround** (`(?=) (?!) (?<=) (?<!)`) and **backreferences**
//!   (`\1` `\k<name>`). These are detected at compile time via `needs_backtrack`.
//!
//! This is the principled split: the fast path keeps its linear-time guarantee,
//! and the rare advanced features fall back to a clean backtracker rather than
//! being rejected (à la RE2).

// ---- AST ------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Ast {
    Empty,
    Char(char),
    Dot,
    Class(Vec<(char, char)>, bool), // ranges, negated
    Start,                          // ^
    End,                            // $
    WordBoundary,                   // \b
    NotWordBoundary,                // \B
    Concat(Vec<Ast>),
    Alt(Vec<Ast>),
    Star(Box<Ast>, bool),           // greedy?
    Plus(Box<Ast>, bool),
    Quest(Box<Ast>, bool),
    Group(Box<Ast>, usize),         // (capturing group number; 0 = non-capturing)
    /// `(?=)`/`(?!)`/`(?<=)`/`(?<!)`: `(ahead, positive)`.
    Look(Box<Ast>, bool, bool),
    /// `\1` / `\k<name>`: backreference to a numbered capture group.
    Backref(usize),
}

// ---- NFA instructions (Pike VM only) --------------------------------------

#[derive(Debug, Clone)]
pub enum Insn {
    Char(char),
    Class(Vec<(char, char)>, bool),
    Dot,
    Start,   // ^ (checks position)
    End,     // $ (checks position)
    WordB,   // \b
    NotWordB,// \B
    Jmp(usize),
    Split(usize, usize),
    Save(usize),
    Match,
}

/// A compiled regex program (AST + optional NFA instructions + flags + groups).
#[derive(Debug, Clone)]
pub struct RegexProgram {
    /// NFA instructions for the Pike VM fast path. Empty when `needs_backtrack`.
    pub insns: Vec<Insn>,
    /// The parsed pattern AST (used by the backtracking fallback path).
    ast: Ast,
    /// Numbered capture groups (group 0 is the whole match).
    pub num_groups: usize,
    /// Named groups: `(name, group_number)` pairs, in declaration order.
    pub names: Vec<(String, usize)>,
    pub ignore_case: bool,
    pub multiline: bool,
    pub dotall: bool,
    /// True iff the pattern uses lookaround or backreferences — selects the
    /// backtracking path; otherwise the Pike VM fast path is used.
    pub needs_backtrack: bool,
}

/// A match result: capture group (start, end) byte-offsets in the input.
#[derive(Debug, Clone)]
pub struct RegexMatch {
    pub captures: Vec<Option<(usize, usize)>>,
}

impl RegexMatch {
    pub fn full_match(&self) -> Option<(usize, usize)> {
        self.captures.get(0).copied().flatten()
    }

    /// Text of a capture group (group 0 = whole match), if it participated.
    pub fn group<'a>(&self, input: &'a str, n: usize) -> Option<&'a str> {
        self.captures.get(n).and_then(|c| {
            c.and_then(|(s, e)| input.get(s..e))
        })
    }
}

// ---- Parser (pattern string → AST) ----------------------------------------

struct Parser<'a> {
    chars: &'a [char],
    pos: usize,
    group_count: usize,
    names: Vec<(String, usize)>,
}

impl<'a> Parser<'a> {
    fn parse(pattern: &'a str) -> Result<(Ast, usize, Vec<(String, usize)>), String> {
        let chars: Vec<char> = pattern.chars().collect();
        let mut p = Parser { chars: &chars, pos: 0, group_count: 0, names: Vec::new() };
        let ast = p.parse_alt()?;
        if p.pos < p.chars.len() {
            return Err(format!("unexpected '{}' at {}", p.chars[p.pos], p.pos));
        }
        Ok((ast, p.group_count, p.names))
    }

    fn peek(&self) -> Option<char> { self.chars.get(self.pos).copied() }
    fn peek2(&self) -> Option<char> { self.chars.get(self.pos + 1).copied() }
    fn bump(&mut self) -> Option<char> { let c = self.peek()?; self.pos += 1; Some(c) }

    /// alternation: concat ('|' concat)*
    fn parse_alt(&mut self) -> Result<Ast, String> {
        let first = self.parse_concat()?;
        if self.peek() != Some('|') { return Ok(first); }
        let mut alts = vec![first];
        while self.peek() == Some('|') {
            self.bump();
            alts.push(self.parse_concat()?);
        }
        Ok(Ast::Alt(alts))
    }

    /// concatenation: quantified*
    fn parse_concat(&mut self) -> Result<Ast, String> {
        let mut parts = Vec::new();
        loop {
            match self.peek() {
                None | Some('|') | Some(')') => break,
                _ => parts.push(self.parse_quant()?),
            }
        }
        if parts.len() == 1 { Ok(parts.pop().unwrap()) }
        else if parts.is_empty() { Ok(Ast::Empty) }
        else { Ok(Ast::Concat(parts)) }
    }

    /// quantified: atom ('*' | '+' | '?' | '{n,m}') ('?')?
    fn parse_quant(&mut self) -> Result<Ast, String> {
        let atom = self.parse_atom()?;
        match self.peek() {
            Some('*') => { self.bump(); let lazy = self.peek() == Some('?'); if lazy { self.bump(); } Ok(Ast::Star(Box::new(atom), !lazy)) }
            Some('+') => { self.bump(); let lazy = self.peek() == Some('?'); if lazy { self.bump(); } Ok(Ast::Plus(Box::new(atom), !lazy)) }
            Some('?') => { self.bump(); let lazy = self.peek() == Some('?'); if lazy { self.bump(); } Ok(Ast::Quest(Box::new(atom), !lazy)) }
            Some('{') => {
                let save = self.pos;
                self.bump(); // {
                if let Some((min, max)) = self.try_braces() {
                    let greedy = self.peek() != Some('?');
                    if !greedy { self.bump(); }
                    Ok(expand_repeat(atom, min, max, greedy))
                } else {
                    // Not a valid `{n,m}` — treat '{' as a literal.
                    self.pos = save;
                    Ok(atom)
                }
            }
            _ => Ok(atom),
        }
    }

    fn try_braces(&mut self) -> Option<(usize, Option<usize>)> {
        let start = self.pos;
        while self.peek().map_or(false, |c| c.is_ascii_digit()) { self.bump(); }
        if self.pos == start { return None; }
        let min: usize = self.chars[start..self.pos].iter().collect::<String>().parse().ok()?;
        let max = if self.peek() == Some(',') {
            self.bump();
            let s = self.pos;
            while self.peek().map_or(false, |c| c.is_ascii_digit()) { self.bump(); }
            if s == self.pos { None } else {
                Some(self.chars[s..self.pos].iter().collect::<String>().parse().ok()?)
            }
        } else {
            Some(min)
        };
        if self.peek() == Some('}') { self.bump(); Some((min, max)) } else { None }
    }

    /// atom: literal | '.' | '[' ... ']' | '(' ... ')' | '\' escape | '^' | '$'
    fn parse_atom(&mut self) -> Result<Ast, String> {
        match self.bump() {
            Some('.') => Ok(Ast::Dot),
            Some('^') => Ok(Ast::Start),
            Some('$') => Ok(Ast::End),
            Some('(') => self.parse_group(),
            Some('[') => self.parse_class(),
            Some('\\') => self.parse_escape(),
            Some(c) => Ok(Ast::Char(c)),
            None => Err("unexpected end".into()),
        }
    }

    fn parse_group(&mut self) -> Result<Ast, String> {
        if self.peek() == Some('?') {
            self.bump();
            match self.peek() {
                Some(':') => { self.bump(); let inner = self.parse_alt()?; self.expect_close()?; Ok(inner) }
                Some('=') => { self.bump(); let inner = self.parse_alt()?; self.expect_close()?; Ok(Ast::Look(Box::new(inner), true, true)) }
                Some('!') => { self.bump(); let inner = self.parse_alt()?; self.expect_close()?; Ok(Ast::Look(Box::new(inner), true, false)) }
                Some('<') => match self.peek2() {
                    Some('=') => { self.bump(); self.bump(); let inner = self.parse_alt()?; self.expect_close()?; Ok(Ast::Look(Box::new(inner), false, true)) }
                    Some('!') => { self.bump(); self.bump(); let inner = self.parse_alt()?; self.expect_close()?; Ok(Ast::Look(Box::new(inner), false, false)) }
                    _ => {
                        // named capturing group: (?<name>...)
                        self.bump(); // consume '<'
                        let name = self.read_group_name()?;
                        self.group_count += 1;
                        let num = self.group_count;
                        self.names.push((name, num));
                        let inner = self.parse_alt()?;
                        self.expect_close()?;
                        Ok(Ast::Group(Box::new(inner), num))
                    }
                },
                Some('P') if self.peek2() == Some('<') => {
                    // Python-style named group (?P<name>...)
                    self.bump(); self.bump(); // P <
                    let name = self.read_group_name()?;
                    self.group_count += 1;
                    let num = self.group_count;
                    self.names.push((name, num));
                    let inner = self.parse_alt()?;
                    self.expect_close()?;
                    Ok(Ast::Group(Box::new(inner), num))
                }
                _ => Err("bad group prefix".into()),
            }
        } else {
            self.group_count += 1;
            let num = self.group_count;
            let inner = self.parse_alt()?;
            self.expect_close()?;
            Ok(Ast::Group(Box::new(inner), num))
        }
    }

    fn expect_close(&mut self) -> Result<(), String> {
        if self.peek() == Some(')') { self.bump(); Ok(()) } else { Err("missing )".into()) }
    }

    /// Read a group name terminated by '>' (after `(?<` / `\k<`).
    fn read_group_name(&mut self) -> Result<String, String> {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c == '>' { break; }
            if c == ')' || c == '(' { return Err("bad group name".into()); }
            self.bump();
        }
        if self.peek() != Some('>') { return Err("unterminated group name".into()); }
        let name: String = self.chars[start..self.pos].iter().collect();
        if name.is_empty() { return Err("empty group name".into()); }
        self.bump(); // '>'
        Ok(name)
    }

    fn parse_class(&mut self) -> Result<Ast, String> {
        let negated = self.peek() == Some('^');
        if negated { self.bump(); }
        let mut ranges = Vec::new();
        let first = self.peek() == Some(']');
        if first { self.bump(); ranges.push((']', ']')); }
        loop {
            match self.peek() {
                None => return Err("unterminated class".into()),
                Some(']') => { self.bump(); break; }
                Some('\\') => {
                    self.bump();
                    let c = self.class_escape_char()?;
                    if self.peek() == Some('-') && self.chars.get(self.pos + 1) != Some(&']') {
                        self.bump();
                        let end = if self.peek() == Some('\\') { self.bump(); self.class_escape_char()? } else { self.bump().unwrap_or('\0') };
                        ranges.push((c, end));
                    } else { ranges.push((c, c)); }
                }
                Some(c) => {
                    self.bump();
                    if self.peek() == Some('-') && self.chars.get(self.pos + 1) != Some(&']') {
                        self.bump();
                        let end = self.bump().unwrap_or(c);
                        ranges.push((c, end));
                    } else {
                        ranges.push((c, c));
                    }
                }
            }
        }
        Ok(Ast::Class(ranges, negated))
    }

    fn class_escape_char(&mut self) -> Result<char, String> {
        let c = self.bump().ok_or("bad class escape")?;
        Ok(match c { 'n' => '\n', 't' => '\t', 'r' => '\r', _ => c })
    }

    fn parse_escape(&mut self) -> Result<Ast, String> {
        let c = self.bump().ok_or("trailing backslash")?;
        match c {
            'd' => Ok(Ast::Class(vec![('0', '9')], false)),
            'D' => Ok(Ast::Class(vec![('0', '9')], true)),
            'w' => Ok(Ast::Class(vec![('a','z'),('A','Z'),('0','9'),('_','_')], false)),
            'W' => Ok(Ast::Class(vec![('a','z'),('A','Z'),('0','9'),('_','_')], true)),
            's' => Ok(Ast::Class(vec![(' ',' '),('\t','\t'),('\n','\n'),('\r','\r'),('\u{000B}','\u{000B}'),('\u{000C}','\u{000C}')], false)),
            'S' => Ok(Ast::Class(vec![(' ',' '),('\t','\t'),('\n','\n'),('\r','\r')], true)),
            'b' => Ok(Ast::WordBoundary),
            'B' => Ok(Ast::NotWordBoundary),
            'n' => Ok(Ast::Char('\n')),
            't' => Ok(Ast::Char('\t')),
            'r' => Ok(Ast::Char('\r')),
            'k' => {
                // named backreference: \k<name> or \k'name'
                let name = self.read_backref_name()?;
                let num = self.lookup_name(&name).ok_or_else(|| format!("unknown group name '{}'", name))?;
                Ok(Ast::Backref(num))
            }
            ch if ch.is_ascii_digit() && ch != '0' => {
                // numeric backreference \1..\9 (possibly multi-digit, take greedy)
                let mut digits = String::from(ch);
                while let Some(d) = self.peek() {
                    if d.is_ascii_digit() { digits.push(d); self.bump(); } else { break; }
                }
                let num: usize = digits.parse().unwrap();
                Ok(Ast::Backref(num))
            }
            _ => Ok(Ast::Char(c)), // identity escape
        }
    }

    fn read_backref_name(&mut self) -> Result<String, String> {
        match self.peek() {
            Some('<') => { self.bump(); self.read_group_name() }
            Some('\'') => {
                self.bump();
                let start = self.pos;
                while let Some(c) = self.peek() { if c == '\'' { break; } self.bump(); }
                if self.peek() != Some('\'') { return Err("unterminated backref name".into()); }
                let name: String = self.chars[start..self.pos].iter().collect();
                self.bump();
                Ok(name)
            }
            _ => Err("expected < or ' after \\k".into()),
        }
    }

    fn lookup_name(&self, name: &str) -> Option<usize> {
        self.names.iter().find(|(n, _)| n == name).map(|(_, n)| *n)
    }
}

/// Expand a bounded repetition `{min, max}` into the existing AST primitives so
/// the Pike VM (linear path) needs no new instructions.
///
/// - `{min}`  → `atom` × min
/// - `{min,}` → `atom` × min, then `atom*` (greedy or lazy)
/// - `{min,max}` → `atom` × min, then `(max - min)` optional `atom`
fn expand_repeat(atom: Ast, min: usize, max: Option<usize>, greedy: bool) -> Ast {
    let mut parts = Vec::new();
    for _ in 0..min { parts.push(atom.clone()); }
    match max {
        None => parts.push(Ast::Star(Box::new(atom), greedy)),
        Some(mx) => {
            let optional = mx.saturating_sub(min);
            for _ in 0..optional { parts.push(Ast::Quest(Box::new(atom.clone()), greedy)); }
        }
    }
    match parts.len() {
        0 => Ast::Empty,
        1 => parts.pop().unwrap(),
        _ => Ast::Concat(parts),
    }
}

/// True iff the AST contains lookaround or backreference nodes (requires the
/// backtracking path).
fn needs_backtrack(ast: &Ast) -> bool {
    match ast {
        Ast::Look(..) | Ast::Backref(_) => true,
        Ast::Empty | Ast::Char(_) | Ast::Dot | Ast::Class(_, _)
        | Ast::Start | Ast::End | Ast::WordBoundary | Ast::NotWordBoundary => false,
        Ast::Concat(xs) | Ast::Alt(xs) => xs.iter().any(needs_backtrack),
        Ast::Star(x, _) | Ast::Plus(x, _) | Ast::Quest(x, _) => needs_backtrack(x),
        Ast::Group(x, _) => needs_backtrack(x),
    }
}

// ---- Compiler (AST → instruction list, Pike VM path) ----------------------

struct Compiler {
    insns: Vec<Insn>,
}

impl Compiler {
    fn new() -> Self { Compiler { insns: Vec::new() } }

    fn emit(&mut self, insn: Insn) -> usize { let i = self.insns.len(); self.insns.push(insn); i }
    fn here(&self) -> usize { self.insns.len() }

    fn compile(&mut self, ast: &Ast) {
        match ast {
            Ast::Empty => {}
            Ast::Char(c) => { self.emit(Insn::Char(*c)); }
            Ast::Dot => { self.emit(Insn::Dot); }
            Ast::Class(r, n) => { self.emit(Insn::Class(r.clone(), *n)); }
            Ast::Start => { self.emit(Insn::Start); }
            Ast::End => { self.emit(Insn::End); }
            Ast::WordBoundary => { self.emit(Insn::WordB); }
            Ast::NotWordBoundary => { self.emit(Insn::NotWordB); }
            Ast::Concat(parts) => { for p in parts { self.compile(p); } }
            Ast::Alt(alts) => {
                let mut jumps = Vec::new();
                for (i, alt) in alts.iter().enumerate() {
                    if i + 1 < alts.len() {
                        let split = self.emit(Insn::Split(0, 0));
                        let l1 = self.here();
                        self.compile(alt);
                        jumps.push(self.emit(Insn::Jmp(0)));
                        let l2 = self.here();
                        self.insns[split] = Insn::Split(l1, l2);
                    } else {
                        self.compile(alt);
                    }
                }
                let end = self.here();
                for j in jumps { self.insns[j] = Insn::Jmp(end); }
            }
            Ast::Star(body, greedy) => {
                let l1 = self.here();
                let split = self.emit(Insn::Split(0, 0));
                let l2 = self.here();
                self.compile(body);
                self.emit(Insn::Jmp(l1));
                let l3 = self.here();
                self.insns[split] = if *greedy { Insn::Split(l2, l3) } else { Insn::Split(l3, l2) };
            }
            Ast::Plus(body, greedy) => {
                let l1 = self.here();
                self.compile(body);
                let split = self.emit(Insn::Split(0, 0));
                let l3 = self.here();
                self.insns[split] = if *greedy { Insn::Split(l1, l3) } else { Insn::Split(l3, l1) };
            }
            Ast::Quest(body, greedy) => {
                let split = self.emit(Insn::Split(0, 0));
                let l1 = self.here();
                self.compile(body);
                let l2 = self.here();
                self.insns[split] = if *greedy { Insn::Split(l1, l2) } else { Insn::Split(l2, l1) };
            }
            Ast::Group(body, n) => {
                self.emit(Insn::Save(2 * n));
                self.compile(body);
                self.emit(Insn::Save(2 * n + 1));
            }
            // Lookaround / backreferences are unreachable on the Pike VM path
            // (needs_backtrack ⇒ backtracking simulator is used instead).
            Ast::Look(..) | Ast::Backref(_) => {}
        }
    }
}

/// Compile a pattern + flags into a [`RegexProgram`].
pub fn compile(pattern: &str, flags: &str) -> Result<RegexProgram, String> {
    let (ast, num_groups, names) = Parser::parse(pattern)?;
    let nb = needs_backtrack(&ast);
    let insns = if nb {
        Vec::new()
    } else {
        let mut c = Compiler::new();
        c.emit(Insn::Save(0));
        c.compile(&ast);
        c.emit(Insn::Save(1));
        c.emit(Insn::Match);
        c.insns
    };
    Ok(RegexProgram {
        insns,
        ast,
        num_groups,
        names,
        ignore_case: flags.contains('i'),
        multiline: flags.contains('m'),
        dotall: flags.contains('s'),
        needs_backtrack: nb,
    })
}

// ---- Execution: dispatch ---------------------------------------------------

impl RegexProgram {
    /// Run the program against `input` starting at byte offset `start`.
    /// Tries each position from `start` onward (unanchored leftmost match).
    pub fn run(&self, input: &str, start: usize) -> Option<RegexMatch> {
        if self.needs_backtrack {
            self.run_backtrack(input, start)
        } else {
            self.run_pike(input, start)
        }
    }

    /// All non-overlapping matches in `input` from `start` onward (leftmost).
    /// Used by `String.prototype.replace` with the global flag.
    pub fn find_all(&self, input: &str, start: usize) -> Vec<RegexMatch> {
        let mut out = Vec::new();
        let mut cur = start;
        let mut prev = usize::MAX;
        while cur <= input.len() && cur != prev {
            prev = cur;
            match self.run(input, cur) {
                None => break,
                Some(m) => {
                    let (s, e) = m.full_match().unwrap_or((cur, cur));
                    out.push(m);
                    cur = if e > s {
                        e
                    } else {
                        // zero-width match: step one char forward
                        input[s..].char_indices().nth(1).map(|(off, _)| s + off).unwrap_or(input.len())
                    };
                }
            }
        }
        out
    }

    fn start_char_index(input: &str, start: usize) -> usize {
        let mut byte = 0;
        for (i, c) in input.chars().enumerate() {
            if byte >= start { return i; }
            byte += c.len_utf8();
        }
        if byte >= start { input.chars().count() } else { 0 }
    }

    // ---- Pike VM (linear path) ---------------------------------------------

    fn run_pike(&self, input: &str, start: usize) -> Option<RegexMatch> {
        let chars: Vec<char> = input.chars().collect();
        let start_idx = Self::start_char_index(input, start);
        for s in start_idx..=chars.len() {
            if let Some(m) = self.pike_chars(&chars, s, input) {
                return Some(m);
            }
        }
        None
    }

    fn pike_chars(&self, chars: &[char], start: usize, input: &str) -> Option<RegexMatch> {
        let n = chars.len();
        let num_slots = (self.num_groups + 1) * 2;
        let vm = VmCtx { insns: &self.insns, chars, n, multiline: self.multiline };

        let mut clist: Vec<(usize, Vec<Option<usize>>)> = Vec::new();
        let mut nlist: Vec<(usize, Vec<Option<usize>>)> = Vec::new();
        let mut cvisited = vec![false; self.insns.len()];
        let mut nvisited = vec![false; self.insns.len()];
        let mut matched: Option<Vec<Option<usize>>> = None;

        vm.add(0, vec![None; num_slots], &mut clist, &mut cvisited, start, &mut matched);

        for pos in start..n {
            let c = chars[pos];
            for &(pc, ref caps) in &clist {
                match &self.insns[pc] {
                    Insn::Char(ch) => {
                        if char_eq(c, *ch, self.ignore_case) {
                            vm.add(pc + 1, caps.clone(), &mut nlist, &mut nvisited, pos + 1, &mut matched);
                        }
                    }
                    Insn::Class(ranges, neg) => {
                        if class_matches(c, ranges, *neg, self.ignore_case) {
                            vm.add(pc + 1, caps.clone(), &mut nlist, &mut nvisited, pos + 1, &mut matched);
                        }
                    }
                    Insn::Dot => {
                        if self.dotall || c != '\n' {
                            vm.add(pc + 1, caps.clone(), &mut nlist, &mut nvisited, pos + 1, &mut matched);
                        }
                    }
                    _ => {}
                }
            }
            std::mem::swap(&mut clist, &mut nlist);
            nlist.clear();
            std::mem::swap(&mut cvisited, &mut nvisited);
            nvisited.fill(false);
            if clist.is_empty() { break; }
        }

        for &(pc, ref caps) in &clist {
            if matches!(self.insns[pc], Insn::Match) {
                matched = Some(caps.clone());
                break;
            }
        }

        matched.map(|caps| build_match(input, chars, &caps))
    }

    // ---- Backtracking simulator (lookaround + backrefs) --------------------

    fn run_backtrack(&self, input: &str, start: usize) -> Option<RegexMatch> {
        let chars: Vec<char> = input.chars().collect();
        let start_idx = Self::start_char_index(input, start);
        let num_slots = (self.num_groups + 1) * 2;
        for s in start_idx..=chars.len() {
            let mut caps: Vec<Option<usize>> = vec![None; num_slots];
            let origin = s;
            // The top continuation records group 0 (whole match) and accepts.
            let done = |p: usize, c: &mut Vec<Option<usize>>| -> Option<usize> {
                c[0] = Some(origin);
                c[1] = Some(p);
                Some(p)
            };
            if let Some(_) = self.match_at(&self.ast, &chars, s, &mut caps, &done) {
                return Some(build_match(input, &chars, &caps));
            }
        }
        None
    }

    /// Continuation-passing backtracking matcher over the AST.
    ///
    /// `cont(end_pos, caps)` represents "the rest of the pattern still to
    /// match" — on a successful overall match it returns `Some(final_pos)`.
    /// Each node tries every way it can match and, for each, hands control to
    /// `cont`. Capture state is mutated in place and restored (via cheap clone
    /// of the small caps vector) when a branch is abandoned.
    fn match_at(
        &self,
        node: &Ast,
        chars: &[char],
        pos: usize,
        caps: &mut Vec<Option<usize>>,
        cont: &dyn Fn(usize, &mut Vec<Option<usize>>) -> Option<usize>,
    ) -> Option<usize> {
        match node {
            Ast::Empty => cont(pos, caps),
            Ast::Char(c) => {
                if pos < chars.len() && char_eq(chars[pos], *c, self.ignore_case) { cont(pos + 1, caps) } else { None }
            }
            Ast::Dot => {
                if pos < chars.len() && (self.dotall || chars[pos] != '\n') { cont(pos + 1, caps) } else { None }
            }
            Ast::Class(r, neg) => {
                if pos < chars.len() && class_matches(chars[pos], r, *neg, self.ignore_case) { cont(pos + 1, caps) } else { None }
            }
            Ast::Start => {
                if pos == 0 || (self.multiline && pos > 0 && chars[pos - 1] == '\n') { cont(pos, caps) } else { None }
            }
            Ast::End => {
                if pos == chars.len() || (self.multiline && pos < chars.len() && chars[pos] == '\n') { cont(pos, caps) } else { None }
            }
            Ast::WordBoundary => {
                if is_boundary(chars, pos) { cont(pos, caps) } else { None }
            }
            Ast::NotWordBoundary => {
                if !is_boundary(chars, pos) { cont(pos, caps) } else { None }
            }
            Ast::Concat(parts) => self.m_concat(parts, 0, chars, pos, caps, cont),
            Ast::Alt(alts) => {
                for a in alts {
                    let saved = caps.clone();
                    if let Some(p) = self.match_at(a, chars, pos, caps, cont) { return Some(p); }
                    *caps = saved;
                }
                None
            }
            Ast::Star(body, greedy) => self.m_star(body, *greedy, chars, pos, caps, cont),
            Ast::Plus(body, greedy) => {
                // One required copy, then a star — guard against empty matches.
                self.match_at(body, chars, pos, caps, &|p, c| {
                    if p > pos { self.m_star(body, *greedy, chars, p, c, cont) } else { cont(p, c) }
                })
            }
            Ast::Quest(body, greedy) => {
                let saved = caps.clone();
                if *greedy {
                    if let Some(p) = self.match_at(body, chars, pos, caps, cont) { return Some(p); }
                    *caps = saved;
                    cont(pos, caps)
                } else {
                    if let Some(p) = cont(pos, caps) { return Some(p); }
                    *caps = saved;
                    self.match_at(body, chars, pos, caps, cont)
                }
            }
            Ast::Group(inner, n) => {
                let slot = 2 * n;
                let save_start = caps.get(slot).copied().flatten();
                if slot < caps.len() { caps[slot] = Some(pos); }
                let result = self.match_at(inner, chars, pos, caps, &|p, c| {
                    let save_end = c.get(slot + 1).copied().flatten();
                    if slot + 1 < c.len() { c[slot + 1] = Some(p); }
                    match cont(p, c) {
                        Some(r) => Some(r),
                        None => { if slot + 1 < c.len() { c[slot + 1] = save_end; } None }
                    }
                });
                if result.is_none() && slot < caps.len() { caps[slot] = save_start; }
                result
            }
            Ast::Look(inner, ahead, positive) => {
                self.m_look(inner, *ahead, *positive, chars, pos, caps, cont)
            }
            Ast::Backref(n) => {
                let slot = 2 * n;
                if slot + 1 >= caps.len() { return None; }
                match (caps.get(slot).copied().flatten(), caps.get(slot + 1).copied().flatten()) {
                    (Some(s), Some(e)) => {
                        let glen = e - s;
                        if pos + glen <= chars.len() && chars[pos..pos + glen] == chars[s..e] {
                            cont(pos + glen, caps)
                        } else { None }
                    }
                    _ => {
                        // Unmatched group: a backreference matches the empty string.
                        cont(pos, caps)
                    }
                }
            }
        }
    }

    fn m_concat(
        &self,
        parts: &[Ast],
        i: usize,
        chars: &[char],
        pos: usize,
        caps: &mut Vec<Option<usize>>,
        cont: &dyn Fn(usize, &mut Vec<Option<usize>>) -> Option<usize>,
    ) -> Option<usize> {
        if i == parts.len() { return cont(pos, caps); }
        self.match_at(&parts[i], chars, pos, caps, &|p, c| {
            self.m_concat(parts, i + 1, chars, p, c, cont)
        })
    }

    /// Greedy/lazy Kleene star with empty-match loop protection: a further
    /// iteration is only attempted when it advanced the position.
    fn m_star(
        &self,
        body: &Ast,
        greedy: bool,
        chars: &[char],
        pos: usize,
        caps: &mut Vec<Option<usize>>,
        cont: &dyn Fn(usize, &mut Vec<Option<usize>>) -> Option<usize>,
    ) -> Option<usize> {
        if greedy {
            let saved = caps.clone();
            let more = self.match_at(body, chars, pos, caps, &|p, c| {
                if p > pos { self.m_star(body, true, chars, p, c, cont) } else { None }
            });
            if more.is_some() { return more; }
            *caps = saved;
            cont(pos, caps)
        } else {
            let saved = caps.clone();
            if let Some(p) = cont(pos, caps) { return Some(p); }
            *caps = saved;
            self.match_at(body, chars, pos, caps, &|p, c| {
                if p > pos { self.m_star(body, false, chars, p, c, cont) } else { None }
            })
        }
    }

    fn m_look(
        &self,
        inner: &Ast,
        ahead: bool,
        positive: bool,
        chars: &[char],
        pos: usize,
        caps: &mut Vec<Option<usize>>,
        cont: &dyn Fn(usize, &mut Vec<Option<usize>>) -> Option<usize>,
    ) -> Option<usize> {
        // Sub-match continuation: succeed and report the position reached. The
        // end position is what lookbehind compares against `pos`.
        let accept = |p: usize, _: &mut Vec<Option<usize>>| Some(p);

        if ahead {
            let saved = caps.clone();
            let matched = self.match_at(inner, chars, pos, caps, &accept);
            if positive {
                // Positive lookahead: inner must match; its captures are kept.
                if matched.is_some() {
                    cont(pos, caps)
                } else {
                    *caps = saved;
                    None
                }
            } else {
                // Negative lookahead: inner must NOT match; discard its captures.
                *caps = saved;
                if matched.is_none() { cont(pos, caps) } else { None }
            }
        } else {
            // lookbehind: find a start `sp` such that inner matches ending
            // exactly at `pos` (i.e. inner's end position == pos).
            if positive {
                for sp in (0..=pos).rev() {
                    let saved = caps.clone();
                    if let Some(ep) = self.match_at(inner, chars, sp, caps, &accept) {
                        if ep == pos {
                            // captures from this trial are kept (JS semantics).
                            return cont(pos, caps);
                        }
                    }
                    *caps = saved;
                }
                None
            } else {
                // Negative lookbehind: succeed iff no start matches ending at pos.
                let mut any = false;
                for sp in 0..=pos {
                    let saved = caps.clone();
                    if let Some(ep) = self.match_at(inner, chars, sp, caps, &accept) {
                        if ep == pos { any = true; break; }
                    }
                    *caps = saved;
                }
                if !any { cont(pos, caps) } else { None }
            }
        }
    }
}

/// Convert a char-offset capture vector into a byte-offset `RegexMatch`.
fn build_match(input: &str, chars: &[char], caps: &[Option<usize>]) -> RegexMatch {
    // Precompute the byte offset of each character boundary (char index → bytes).
    let mut byte_of = Vec::with_capacity(chars.len() + 1);
    byte_of.push(0);
    let mut b = 0;
    for c in chars { b += c.len_utf8(); byte_of.push(b); }
    let to_byte = |ci: usize| if ci < byte_of.len() { byte_of[ci] } else { input.len() };

    let captures = caps
        .chunks(2)
        .map(|chunk| match (chunk[0], chunk[1]) {
            (Some(s), Some(e)) => Some((to_byte(s), to_byte(e))),
            _ => None,
        })
        .collect();
    RegexMatch { captures }
}

// ---- Pike VM thread runner (follows non-consuming instructions) -----------

struct VmCtx<'a> {
    insns: &'a [Insn],
    chars: &'a [char],
    n: usize,
    multiline: bool,
}

impl<'a> VmCtx<'a> {
    fn add(
        &self,
        mut pc: usize,
        mut caps: Vec<Option<usize>>,
        list: &mut Vec<(usize, Vec<Option<usize>>)>,
        visited: &mut [bool],
        pos: usize,
        matched: &mut Option<Vec<Option<usize>>>,
    ) {
        loop {
            if pc >= self.insns.len() || visited[pc] { break; }
            visited[pc] = true;
            match &self.insns[pc] {
                Insn::Jmp(x) => { pc = *x; }
                Insn::Split(a, b) => {
                    let caps_b = caps.clone();
                    self.add(*a, caps, list, visited, pos, matched);
                    self.add(*b, caps_b, list, visited, pos, matched);
                    return;
                }
                Insn::Save(slot) => {
                    if *slot < caps.len() { caps[*slot] = Some(pos); }
                    pc += 1;
                }
                Insn::Match => {
                    *matched = Some(caps.clone());
                    return;
                }
                Insn::Start => {
                    if pos == 0 || (self.multiline && pos > 0 && self.chars[pos - 1] == '\n') {
                        pc += 1;
                    } else { return; }
                }
                Insn::End => {
                    if pos == self.n || (self.multiline && pos < self.n && self.chars[pos] == '\n') {
                        pc += 1;
                    } else { return; }
                }
                Insn::WordB => {
                    if is_boundary(self.chars, pos) { pc += 1; } else { return; }
                }
                Insn::NotWordB => {
                    if !is_boundary(self.chars, pos) { pc += 1; } else { return; }
                }
                _ => { list.push((pc, caps)); return; }
            }
        }
    }
}

// ---- Shared helpers -------------------------------------------------------

fn is_boundary(chars: &[char], pos: usize) -> bool {
    let prev = pos > 0 && is_word_char(chars[pos - 1]);
    let curr = pos < chars.len() && is_word_char(chars[pos]);
    prev != curr
}

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn char_eq(a: char, b: char, ignore_case: bool) -> bool {
    if a == b { return true; }
    if ignore_case {
        a.to_ascii_lowercase() == b.to_ascii_lowercase()
    } else { false }
}

fn class_matches(c: char, ranges: &[(char, char)], negated: bool, ignore_case: bool) -> bool {
    let mut found = false;
    for &(lo, hi) in ranges {
        if c >= lo && c <= hi { found = true; break; }
        if ignore_case {
            let cl = c.to_ascii_lowercase();
            let cu = c.to_ascii_uppercase();
            if (cl >= lo && cl <= hi) || (cu >= lo && cu <= hi) { found = true; break; }
        }
    }
    found != negated
}

// ---- Tests ----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn m(pattern: &str, input: &str) -> bool {
        compile(pattern, "").unwrap().run(input, 0).is_some()
    }

    #[test]
    fn literals() {
        assert!(m("abc", "abc"));
        assert!(!m("abc", "abd"));
        assert!(m("abc", "xabcx"));
    }

    #[test]
    fn dot_star() {
        assert!(m("a.c", "abc"));
        assert!(m("a.*c", "axxxc"));
        assert!(!m("a.*c", "axxxd"));
    }

    #[test]
    fn alternation() {
        assert!(m("cat|dog", "cat"));
        assert!(m("cat|dog", "dog"));
        assert!(!m("cat|dog", "bird"));
    }

    #[test]
    fn quantifiers() {
        assert!(m("ab+c", "abbbc"));
        assert!(!m("ab+c", "ac"));
        assert!(m("ab?c", "ac"));
        assert!(m("ab?c", "abc"));
    }

    #[test]
    fn classes() {
        assert!(m("\\d+", "12345"));
        assert!(!m("\\d+", "abc"));
        assert!(m("[a-z]+", "hello"));
        assert!(m("[^0-9]+", "abc"));
        assert!(m("\\w+", "hello_world123"));
    }

    #[test]
    fn anchors() {
        assert!(m("^abc", "abc"));
        assert!(!m("^abc", "xabc"));
        assert!(m("abc$", "xabc"));
        assert!(!m("abc$", "abcx"));
    }

    #[test]
    fn groups() {
        let prog = compile("(\\d+)-(\\d+)", "").unwrap();
        let r = prog.run("12-34", 0).unwrap();
        assert_eq!(r.captures[0], Some((0, 5)));
        assert_eq!(r.captures[1], Some((0, 2)));
        assert_eq!(r.captures[2], Some((3, 5)));
    }

    #[test]
    fn case_insensitive() {
        let prog = compile("hello", "i").unwrap();
        assert!(prog.run("HELLO", 0).is_some());
        assert!(prog.run("HeLLo", 0).is_some());
    }

    // ---- new feature tests ----

    #[test]
    fn braces() {
        assert!(m("a{3}", "aaa"));
        assert!(!m("a{3}", "aa"));
        assert!(m("a{2,4}", "aaaa"));
        assert!(m("a{2,4}", "aa"));
        assert!(!m("a{2,4}", "a"));
        assert!(m("a{2,}", "aaaaa"));
        assert!(!m("a{2,}", "a"));
        // expansion stays on the linear (Pike VM) path
        let prog = compile("a{2,3}", "").unwrap();
        assert!(!prog.needs_backtrack);
        assert!(prog.run("aaa", 0).is_some());
    }

    #[test]
    fn braces_lazy_and_capture() {
        let prog = compile("(a{2,})", "").unwrap();
        let r = prog.run("aaaa", 0).unwrap();
        assert_eq!(r.group("aaaa", 1), Some("aaaa"));
    }

    #[test]
    fn lookahead_positive() {
        let prog = compile("a(?=b)", "").unwrap();
        assert!(prog.needs_backtrack);
        // matches the 'a' only when followed by 'b'; consumes just 'a'
        let r = prog.run("xab", 0).unwrap();
        assert_eq!(r.full_match(), Some((1, 2)));
        assert!(prog.run("xac", 0).is_none());
    }

    #[test]
    fn lookahead_negative() {
        let prog = compile("a(?!b)", "").unwrap();
        assert!(prog.run("xac", 0).is_some());
        assert!(prog.run("xab", 0).is_none());
    }

    #[test]
    fn lookbehind_positive() {
        let prog = compile("(?<=a)b", "").unwrap();
        assert!(prog.needs_backtrack);
        let r = prog.run("xab", 0).unwrap();
        assert_eq!(r.full_match(), Some((2, 3))); // matches the 'b'
        assert!(prog.run("xb", 0).is_none());
    }

    #[test]
    fn lookbehind_negative() {
        let prog = compile("(?<!a)b", "").unwrap();
        assert!(prog.run("xb", 0).is_some());
        assert!(prog.run("ab", 0).is_none());
    }

    #[test]
    fn backref_numeric() {
        // a word, then the same word again
        let prog = compile("(\\w+) \\1", "").unwrap();
        assert!(prog.needs_backtrack);
        assert!(prog.run("hi hi", 0).is_some());
        assert!(prog.run("hi ho", 0).is_none());
    }

    #[test]
    fn backref_named() {
        let prog = compile("(?<word>\\w+)-\\k<word>", "").unwrap();
        assert!(prog.run("go-go", 0).is_some());
        assert!(prog.run("go-stop", 0).is_none());
    }

    #[test]
    fn lookahead_captures() {
        // captures inside positive lookahead are preserved
        let prog = compile("(?=(\\d+))", "").unwrap();
        let r = prog.run("abc123", 0).unwrap();
        assert_eq!(r.group("abc123", 1), Some("123"));
    }

    #[test]
    fn find_all_global() {
        let prog = compile("\\d+", "g").unwrap();
        let ms = prog.find_all("a1 b22 c333", 0);
        assert_eq!(ms.len(), 3);
        assert_eq!(ms[0].full_match(), Some((1, 2)));
        assert_eq!(ms[1].full_match(), Some((4, 6)));
        assert_eq!(ms[2].full_match(), Some((8, 11)));
    }
}
