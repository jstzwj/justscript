

pub enum Keyword {
    Break,
    Case,
    Catch,
    Continue,
}

/*
'default' | 'delete' | 'do'
	| 'else' | 'finally' | 'for' | 'function' | 'if' | 'in' | 'instanceof'
	| 'new' | 'return' | 'switch' | 'this' | 'throw' | 'try' | 'typeof'
    | 'var' | 'void' | 'while' | 'with'
    */
    
impl FromStr for Keyword {
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match {
            "break" => Keyword::Break,
            "case" => Keyword::Case,
            "catch" => Keyword::Catch,
            "continue" => Keyword::Continue,
        }
    }
}