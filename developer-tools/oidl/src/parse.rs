//! The parser (`docs/spec/OIDL.md` §2.1): recursive descent over tokens, with a depth limit on
//! the one recursive production, so untrusted input cannot exhaust the stack.

use crate::ast::*;
use crate::diag::{Code, Diagnostic};
use crate::lex::{self, Punct, Spanned, Token};
use crate::names;

/// Deepest type nesting (§2).
pub const MAX_TYPE_DEPTH: usize = 8;
/// Most `use` lines in a library (§2).
pub const MAX_USES: usize = 64;
/// Most items in a library (§2).
pub const MAX_ITEMS: usize = 4096;
/// Most members of an enum or bits (§2).
pub const MAX_MEMBERS: usize = 4096;
/// Most fields of a struct (§2).
pub const MAX_FIELDS: usize = 1024;
/// Most methods of an interface (§2).
pub const MAX_METHODS: usize = 4096;
/// Most parameters, or results, of a method (§2).
pub const MAX_PARAMS: usize = 64;
/// Most attributes on one declaration (§2).
pub const MAX_ATTRS: usize = 8;

struct Parser<'a> {
    file: &'a str,
    tokens: &'a [Spanned],
    at: usize,
}

type Parsed<T> = Result<T, Diagnostic>;

impl Parser<'_> {
    fn current(&self) -> Option<&Spanned> {
        self.tokens.get(self.at)
    }

    fn token(&self) -> &Token {
        self.current().map_or(&Token::Eof, |s| &s.token)
    }

    fn pos(&self) -> Pos {
        self.current().map_or(Pos { line: 1, column: 1 }, |s| Pos {
            line: s.line,
            column: s.column,
        })
    }

    fn advance(&mut self) {
        self.at = self.at.saturating_add(1);
    }

    fn fail_at<T>(&self, pos: Pos, code: Code, message: impl Into<String>) -> Parsed<T> {
        Err(Diagnostic {
            code,
            file: self.file.to_string(),
            line: pos.line,
            column: pos.column,
            message: message.into(),
        })
    }

    fn fail<T>(&self, code: Code, message: impl Into<String>) -> Parsed<T> {
        self.fail_at(self.pos(), code, message)
    }

    fn unexpected<T>(&self, wanted: &str) -> Parsed<T> {
        self.fail(
            Code::UnexpectedToken,
            format!("{wanted} expected, found {:?}", self.token()),
        )
    }

    /// Refuses one more of something already at its limit (§2).
    fn check_count(&self, count: usize, max: usize, what: &str) -> Parsed<()> {
        if count >= max {
            self.fail(Code::TooMany, format!("more than {max} {what}"))
        } else {
            Ok(())
        }
    }

    fn is_punct(&self, punct: Punct) -> bool {
        matches!(self.token(), Token::Punct(p) if *p == punct)
    }

    fn eat_punct(&mut self, punct: Punct) -> bool {
        let found = self.is_punct(punct);
        if found {
            self.advance();
        }
        found
    }

    fn expect_punct(&mut self, punct: Punct) -> Parsed<()> {
        if self.eat_punct(punct) {
            Ok(())
        } else {
            self.unexpected(&format!("{punct:?}"))
        }
    }

    fn is_word(&self, word: &str) -> bool {
        matches!(self.token(), Token::Ident(t) if t == word)
    }

    fn eat_word(&mut self, word: &str) -> bool {
        let found = self.is_word(word);
        if found {
            self.advance();
        }
        found
    }

    fn expect_word(&mut self, word: &str) -> Parsed<()> {
        if self.eat_word(word) {
            Ok(())
        } else {
            self.unexpected(word)
        }
    }

    /// A name a declaration gives to something: never an OIDL keyword.
    fn ident(&mut self) -> Parsed<Ident> {
        let pos = self.pos();
        match self.token().clone() {
            Token::Ident(text) => {
                if names::is_reserved(&text) {
                    return self.fail(Code::ReservedWord, text);
                }
                self.advance();
                Ok(Ident { text, pos })
            }
            _ => self.unexpected("a name"),
        }
    }

    fn word_any(&mut self) -> Parsed<Ident> {
        let pos = self.pos();
        match self.token().clone() {
            Token::Ident(text) => {
                self.advance();
                Ok(Ident { text, pos })
            }
            _ => self.unexpected("a word"),
        }
    }

    fn number(&mut self) -> Parsed<u64> {
        match *self.token() {
            Token::Number(value) => {
                self.advance();
                Ok(value)
            }
            _ => self.unexpected("a number"),
        }
    }

    fn dotted(&mut self) -> Parsed<Dotted> {
        let pos = self.pos();
        let first = self.ident()?;
        let mut parts = vec![first.text];
        while self.eat_punct(Punct::Dot) {
            parts.push(self.ident()?.text);
        }
        Ok(Dotted { parts, pos })
    }

    fn name(&mut self) -> Parsed<Name> {
        let pos = self.pos();
        let first = self.word_any()?;
        if self.eat_punct(Punct::Dot) {
            let item = self.word_any()?;
            Ok(Name {
                qualifier: Some(first.text),
                item: item.text,
                pos,
            })
        } else {
            Ok(Name {
                qualifier: None,
                item: first.text,
                pos,
            })
        }
    }

    fn docs(&mut self) -> Vec<String> {
        let mut docs = Vec::new();
        while let Token::Doc(text) = self.token() {
            docs.push(text.clone());
            self.advance();
        }
        docs
    }

    fn required_docs(&mut self, what: &str) -> Parsed<Vec<String>> {
        let pos = self.pos();
        let docs = self.docs();
        if docs.is_empty() {
            return self.fail_at(pos, Code::MissingDoc, what);
        }
        Ok(docs)
    }

    fn attrs(&mut self) -> Parsed<Vec<Attr>> {
        let mut attrs = Vec::new();
        while self.is_punct(Punct::At) {
            self.check_count(attrs.len(), MAX_ATTRS, "attributes")?;
            let pos = self.pos();
            self.advance();
            let name = self.word_any()?;
            let mut args = Vec::new();
            if self.eat_punct(Punct::LParen) && !self.eat_punct(Punct::RParen) {
                loop {
                    args.push(self.attr_arg()?);
                    if self.eat_punct(Punct::RParen) {
                        break;
                    }
                    self.expect_punct(Punct::Comma)?;
                }
            }
            attrs.push(Attr {
                name: name.text,
                args,
                pos,
            });
        }
        Ok(attrs)
    }

    fn attr_arg(&mut self) -> Parsed<AttrArg> {
        if matches!(self.token(), Token::Number(_)) {
            let major = self.number()?;
            self.expect_punct(Punct::Dot)?;
            let minor = self.number()?;
            Ok(AttrArg::Version(major, minor))
        } else {
            Ok(AttrArg::Path(self.dotted()?.parts))
        }
    }

    fn bound(&mut self) -> Parsed<Bound> {
        let pos = self.pos();
        if matches!(self.token(), Token::Number(_)) {
            Ok(Bound::Number(self.number()?, pos))
        } else {
            Ok(Bound::Name(self.name()?))
        }
    }

    fn handle_args(&mut self, optional: bool, pos: Pos) -> Parsed<TypeExpr> {
        self.expect_punct(Punct::Lt)?;
        let object = self.ident()?;
        let mut rights = Vec::new();
        if self.eat_punct(Punct::Comma) {
            rights.push(self.ident()?);
            while self.eat_punct(Punct::Pipe) {
                rights.push(self.ident()?);
            }
        }
        self.expect_punct(Punct::Gt)?;
        Ok(TypeExpr::Handle {
            object,
            rights,
            optional,
            pos,
        })
    }

    fn type_expr(&mut self, depth: usize) -> Parsed<TypeExpr> {
        if depth > MAX_TYPE_DEPTH {
            return self.fail(Code::NestingTooDeep, "");
        }
        let pos = self.pos();
        if self.eat_word("bool") {
            return Ok(TypeExpr::Bool(pos));
        }
        if self.eat_word("handle") {
            return self.handle_args(false, pos);
        }
        if self.eat_word("optional") {
            self.expect_punct(Punct::Lt)?;
            self.expect_word("handle")?;
            let handle = self.handle_args(true, pos)?;
            self.expect_punct(Punct::Gt)?;
            return Ok(handle);
        }
        if self.eat_word("array") {
            self.expect_punct(Punct::Lt)?;
            let inner = self.type_expr(depth.saturating_add(1))?;
            self.expect_punct(Punct::Comma)?;
            let bound = self.bound()?;
            self.expect_punct(Punct::Gt)?;
            return Ok(TypeExpr::Array(Box::new(inner), bound, pos));
        }
        if self.eat_word("vector") {
            self.expect_punct(Punct::Lt)?;
            let inner = self.type_expr(depth.saturating_add(1))?;
            self.expect_punct(Punct::Comma)?;
            let bound = self.bound()?;
            self.expect_punct(Punct::Gt)?;
            return Ok(TypeExpr::Vector(Box::new(inner), bound, pos));
        }
        if self.eat_word("string") {
            self.expect_punct(Punct::Lt)?;
            let bound = self.bound()?;
            self.expect_punct(Punct::Gt)?;
            return Ok(TypeExpr::String(bound, pos));
        }
        if matches!(self.token(), Token::Ident(_)) {
            return Ok(TypeExpr::Named(self.name()?));
        }
        self.unexpected("a type")
    }

    fn member(&mut self) -> Parsed<MemberDecl> {
        let doc = self.docs();
        let attrs = self.attrs()?;
        let name = self.ident()?;
        self.expect_punct(Punct::Eq)?;
        let negative = self.eat_punct(Punct::Minus);
        let value = self.number()?;
        self.expect_punct(Punct::Semi)?;
        Ok(MemberDecl {
            doc,
            attrs,
            name,
            negative,
            value,
        })
    }

    fn members(&mut self) -> Parsed<Vec<MemberDecl>> {
        self.expect_punct(Punct::LBrace)?;
        let mut members = Vec::new();
        while !self.eat_punct(Punct::RBrace) {
            self.check_count(members.len(), MAX_MEMBERS, "members")?;
            members.push(self.member()?);
        }
        Ok(members)
    }

    fn values_decl(&mut self, doc: Vec<String>, attrs: Vec<Attr>) -> Parsed<ValuesDecl> {
        let name = self.ident()?;
        self.expect_punct(Punct::Colon)?;
        let repr = self.name()?;
        let members = self.members()?;
        Ok(ValuesDecl {
            doc,
            attrs,
            name,
            repr,
            members,
        })
    }

    fn const_decl(&mut self, doc: Vec<String>, attrs: Vec<Attr>) -> Parsed<ConstDecl> {
        let name = self.ident()?;
        self.expect_punct(Punct::Colon)?;
        let ty = self.name()?;
        self.expect_punct(Punct::Eq)?;
        let pos = self.pos();
        let value = if matches!(self.token(), Token::Ident(_)) {
            let mut bits = vec![self.ident()?];
            while self.eat_punct(Punct::Pipe) {
                bits.push(self.ident()?);
            }
            ConstValue::Bits(bits)
        } else {
            let negative = self.eat_punct(Punct::Minus);
            let value = self.number()?;
            ConstValue::Number {
                negative,
                value,
                pos,
            }
        };
        self.expect_punct(Punct::Semi)?;
        Ok(ConstDecl {
            doc,
            attrs,
            name,
            ty,
            value,
        })
    }

    fn struct_decl(
        &mut self,
        doc: Vec<String>,
        attrs: Vec<Attr>,
        sized: bool,
    ) -> Parsed<StructDecl> {
        let name = self.ident()?;
        self.expect_punct(Punct::LBrace)?;
        let mut fields = Vec::new();
        while !self.eat_punct(Punct::RBrace) {
            self.check_count(fields.len(), MAX_FIELDS, "fields")?;
            let doc = self.docs();
            let attrs = self.attrs()?;
            let name = self.ident()?;
            self.expect_punct(Punct::Colon)?;
            let ty = self.type_expr(0)?;
            self.expect_punct(Punct::Semi)?;
            fields.push(FieldDecl {
                doc,
                attrs,
                name,
                ty,
            });
        }
        Ok(StructDecl {
            doc,
            attrs,
            sized,
            name,
            fields,
        })
    }

    fn param(&mut self) -> Parsed<ParamDecl> {
        let doc = self.docs();
        let attrs = self.attrs()?;
        let name = self.ident()?;
        self.expect_punct(Punct::Colon)?;
        let ty = self.type_expr(0)?;
        Ok(ParamDecl {
            doc,
            attrs,
            name,
            ty,
        })
    }

    fn params(&mut self) -> Parsed<Vec<ParamDecl>> {
        self.expect_punct(Punct::LParen)?;
        let mut params = Vec::new();
        while !self.eat_punct(Punct::RParen) {
            self.check_count(params.len(), MAX_PARAMS, "parameters")?;
            params.push(self.param()?);
            if !self.eat_punct(Punct::Comma) {
                self.expect_punct(Punct::RParen)?;
                break;
            }
        }
        Ok(params)
    }

    fn method(&mut self) -> Parsed<MethodDecl> {
        let pos = self.pos();
        let doc = self.required_docs("a method needs a doc comment")?;
        let attrs = self.attrs()?;
        let oneway = self.eat_word("oneway");
        self.expect_word("method")?;
        let name = self.ident()?;
        self.expect_punct(Punct::Eq)?;
        let ordinal = self.number()?;
        let params = self.params()?;
        let returns = if self.is_punct(Punct::Arrow) {
            self.advance();
            let never_pos = self.pos();
            if self.eat_word("never") {
                Returns::Never(never_pos)
            } else {
                Returns::Values(self.params()?)
            }
        } else {
            Returns::None
        };
        let errors = if self.eat_word("errors") {
            self.expect_punct(Punct::LParen)?;
            let mut list = Vec::new();
            while !self.eat_punct(Punct::RParen) {
                list.push(self.ident()?);
                if !self.eat_punct(Punct::Comma) {
                    self.expect_punct(Punct::RParen)?;
                    break;
                }
            }
            Some(list)
        } else {
            None
        };
        self.expect_word("requires")?;
        let requires = if self.eat_word("none") {
            Requires::None
        } else {
            let name = self.dotted()?;
            let otherwise = if self.eat_word("else") {
                Some(self.ident()?)
            } else {
                None
            };
            Requires::Capability { name, otherwise }
        };
        self.expect_punct(Punct::Semi)?;
        Ok(MethodDecl {
            doc,
            attrs,
            oneway,
            name,
            ordinal,
            params,
            returns,
            errors,
            requires,
            pos,
        })
    }

    fn interface(&mut self, doc: Vec<String>) -> Parsed<InterfaceDecl> {
        let name = self.ident()?;
        self.expect_punct(Punct::LBrace)?;
        let mut methods = Vec::new();
        while !self.eat_punct(Punct::RBrace) {
            self.check_count(methods.len(), MAX_METHODS, "methods")?;
            methods.push(self.method()?);
        }
        Ok(InterfaceDecl { doc, name, methods })
    }

    fn item(&mut self) -> Parsed<Item> {
        let doc = self.required_docs("a declaration needs a doc comment")?;
        let attrs = self.attrs()?;
        if self.eat_word("const") {
            return Ok(Item::Const(self.const_decl(doc, attrs)?));
        }
        if self.eat_word("enum") {
            return Ok(Item::Enum(self.values_decl(doc, attrs)?));
        }
        if self.eat_word("bits") {
            return Ok(Item::Bits(self.values_decl(doc, attrs)?));
        }
        if self.eat_word("sized") {
            self.expect_word("struct")?;
            return Ok(Item::Struct(self.struct_decl(doc, attrs, true)?));
        }
        if self.eat_word("struct") {
            return Ok(Item::Struct(self.struct_decl(doc, attrs, false)?));
        }
        if self.eat_word("capability") {
            let name = self.dotted()?;
            self.expect_punct(Punct::Semi)?;
            return Ok(Item::Capability(CapabilityDecl { doc, attrs, name }));
        }
        if self.is_word("interface") {
            if let Some(attr) = attrs.first() {
                return self.fail_at(
                    attr.pos,
                    Code::MisplacedAttribute,
                    "an interface has no attributes; its methods do",
                );
            }
            self.advance();
            return Ok(Item::Interface(self.interface(doc)?));
        }
        self.unexpected("const, enum, bits, struct, capability or interface")
    }

    fn file(&mut self, file_name: &str) -> Parsed<LibraryFile> {
        let doc = self.required_docs("a library needs a doc comment")?;
        self.expect_word("library")?;
        let name = self.dotted()?;
        self.expect_punct(Punct::Semi)?;
        self.expect_word("transport")?;
        let transport = if self.eat_word("syscall") {
            Transport::Syscall
        } else if self.eat_word("channel") {
            Transport::Channel
        } else {
            return self.unexpected("syscall or channel");
        };
        self.expect_punct(Punct::Semi)?;
        self.expect_word("version")?;
        let pos = self.pos();
        let version = if self.eat_word("compatibility") {
            match self.token().clone() {
                Token::Str(key) => {
                    self.advance();
                    VersionDecl::Key(key, pos)
                }
                _ => return self.unexpected("a compatibility key in quotes"),
            }
        } else if matches!(self.token(), Token::Number(_)) {
            let major = self.number()?;
            self.expect_punct(Punct::Dot)?;
            let minor = self.number()?;
            VersionDecl::Literal(major, minor, pos)
        } else {
            return self.fail(Code::BadVersion, "major.minor or compatibility \"key\"");
        };
        self.expect_punct(Punct::Semi)?;
        let mut uses = Vec::new();
        while self.eat_word("use") {
            self.check_count(uses.len(), MAX_USES, "use lines")?;
            uses.push(self.dotted()?);
            self.expect_punct(Punct::Semi)?;
        }
        let mut items = Vec::new();
        while !matches!(self.token(), Token::Eof) {
            self.check_count(items.len(), MAX_ITEMS, "items")?;
            items.push(self.item()?);
        }
        Ok(LibraryFile {
            file: file_name.to_string(),
            doc,
            name,
            transport,
            version,
            uses,
            items,
        })
    }
}

/// Parses one library file.
pub fn parse(file: &str, source: &[u8]) -> Result<LibraryFile, Diagnostic> {
    let tokens = lex::lex(file, source)?;
    let mut parser = Parser {
        file,
        tokens: &tokens,
        at: 0,
    };
    parser.file(file)
}
