//! Simula text frames (Standard Chapter 8).
//!
//! A text value references a subframe of a shared `TextObject` buffer, or is **notext**.

use std::cell::RefCell;
use std::rc::Rc;

/// Shared character buffer (`TEXTOBJ.MAIN`) with constant flag.
#[derive(Debug)]
pub struct TextObject {
    pub main: RefCell<Vec<char>>,
    pub constant: bool,
}

/// A text frame: optional object reference plus 1-based Simula indices.
#[derive(Debug, Clone)]
pub struct TextFrame {
    pub obj: Option<Rc<TextObject>>,
    /// 1-based index of first character in `obj.main`.
    pub start: i64,
    pub length: i64,
    /// 1-based position indicator; meaningful range is `(1, length + 1)`.
    pub pos: i64,
}

impl TextFrame {
    pub fn notext() -> Self {
        // Standard Chapter 8: NOTEXT has LENGTH=0, START=1, POS=1, CONSTANT=true.
        Self {
            obj: None,
            start: 1,
            length: 0,
            pos: 1,
        }
    }

    pub fn with_pos(&self, pos: i64) -> Self {
        Self {
            obj: self.obj.clone(),
            start: self.start,
            length: self.length,
            pos,
        }
    }

    pub fn is_notext(&self) -> bool {
        self.obj.is_none() || self.length == 0
    }

    pub fn constant(&self) -> bool {
        // NOTEXT is constant; otherwise read the shared TEXTOBJ flag.
        self.is_notext() || self.obj.as_ref().is_some_and(|obj| obj.constant)
    }

    pub fn from_literal(content: &str, constant: bool) -> Self {
        if content.is_empty() {
            return Self::notext();
        }
        Self {
            obj: Some(Rc::new(TextObject {
                main: RefCell::new(content.chars().collect()),
                constant,
            })),
            start: 1,
            length: content.chars().count() as i64,
            pos: 1,
        }
    }

    pub fn from_mutable(content: &str) -> Self {
        Self::from_literal(content, false)
    }

    pub fn blanks(n: i64) -> Result<Self, String> {
        if n < 0 {
            return Err("parameter to blanks < 0".into());
        }
        if n == 0 {
            return Ok(Self::notext());
        }
        Ok(Self {
            obj: Some(Rc::new(TextObject {
                main: RefCell::new(vec![' '; n as usize]),
                constant: false,
            })),
            start: 1,
            length: n,
            pos: 1,
        })
    }

    pub fn copy(source: &Self) -> Self {
        if source.is_notext() {
            return Self::notext();
        }
        let content = source.content();
        Self::from_mutable(&content)
    }

    pub fn upcase_in_place(&mut self) -> Result<(), String> {
        if self.is_notext() || self.constant() {
            return Err("upcase on notext or constant text".into());
        }
        let Some(obj) = &self.obj else {
            return Ok(());
        };
        let mut main = obj.main.borrow_mut();
        let start = (self.start - 1) as usize;
        let end = start + self.length as usize;
        for ch in &mut main[start..end] {
            *ch = ch.to_ascii_uppercase();
        }
        Ok(())
    }

    pub fn lowcase_in_place(&mut self) -> Result<(), String> {
        if self.is_notext() || self.constant() {
            return Err("lowcase on notext or constant text".into());
        }
        let Some(obj) = &self.obj else {
            return Ok(());
        };
        let mut main = obj.main.borrow_mut();
        let start = (self.start - 1) as usize;
        let end = start + self.length as usize;
        for ch in &mut main[start..end] {
            *ch = ch.to_ascii_lowercase();
        }
        Ok(())
    }

    pub fn content(&self) -> String {
        if self.is_notext() {
            return String::new();
        }
        let Some(obj) = &self.obj else {
            return String::new();
        };
        let main = obj.main.borrow();
        let start_idx = (self.start - 1) as usize;
        main[start_idx..start_idx + self.length as usize]
            .iter()
            .collect()
    }

    pub fn main_frame(&self) -> Self {
        if self.is_notext() {
            return Self::notext();
        }
        let Some(obj) = self.obj.clone() else {
            return Self::notext();
        };
        let size = obj.main.borrow().len() as i64;
        Self {
            obj: Some(obj),
            start: 1,
            length: size,
            pos: 1,
        }
    }

    pub fn subframe(&self, i: i64, n: i64) -> Result<Self, String> {
        if i < 0 || n < 0 || i + n > self.length + 1 {
            return Err("sub out of frame".into());
        }
        if n == 0 {
            return Ok(Self::notext());
        }
        Ok(Self {
            obj: self.obj.clone(),
            start: self.start + i - 1,
            length: n,
            pos: 1,
        })
    }

    pub fn strip(&self) -> Self {
        if self.is_notext() {
            return Self::notext();
        }
        let content = self.content();
        let trimmed_len = content.trim_end().chars().count() as i64;
        if trimmed_len == 0 {
            return Self::notext();
        }
        self.subframe(1, trimmed_len)
            .expect("strip subframe is in bounds")
    }

    pub fn setpos(&mut self, i: i64) {
        if i < 1 || i > self.length + 1 {
            self.pos = self.length + 1;
        } else {
            self.pos = i;
        }
    }

    pub fn more(&self) -> bool {
        self.pos <= self.length
    }

    pub fn getchar(&mut self) -> Result<char, String> {
        if self.pos > self.length {
            return Err("pos out of range".into());
        }
        let ch = self.char_at(self.pos)?;
        self.pos += 1;
        Ok(ch)
    }

    pub fn putchar(&mut self, ch: char) -> Result<(), String> {
        if self.is_notext() || self.constant() {
            return Err("putchar on notext or constant text".into());
        }
        if self.pos > self.length {
            return Err("pos out of range".into());
        }
        let Some(obj) = &self.obj else {
            return Err("putchar on notext".into());
        };
        let index = (self.start + self.pos - 2) as usize;
        obj.main.borrow_mut()[index] = ch;
        self.pos += 1;
        Ok(())
    }

    pub fn char_at(&self, pos: i64) -> Result<char, String> {
        if pos < 1 || pos > self.length {
            return Err("pos out of range".into());
        }
        let Some(obj) = &self.obj else {
            return Err("pos out of range".into());
        };
        let index = (self.start + pos - 2) as usize;
        Ok(obj.main.borrow()[index])
    }

    pub fn assign_value_from(&mut self, source: &Self) -> Result<(), String> {
        if self.is_notext() && source.is_notext() {
            self.pos = 1;
            return Ok(());
        }
        if self.is_notext() {
            *self = source.clone();
            return Ok(());
        }
        if source.content().len() as i64 > self.length {
            return Err("text assignment exceeds destination length".into());
        }
        if self.constant() {
            return Err("assignment to constant text frame".into());
        }
        let source_content = source.content();
        let mut padded = source_content;
        padded.push_str(&" ".repeat(self.length as usize - padded.chars().count()));
        self.write_content(&padded)
    }

    fn write_content(&self, content: &str) -> Result<(), String> {
        let Some(obj) = &self.obj else {
            return Ok(());
        };
        let mut chars = content.chars();
        let mut main = obj.main.borrow_mut();
        for index in (self.start - 1) as usize..(self.start - 1 + self.length) as usize {
            main[index] = chars.next().unwrap_or(' ');
        }
        Ok(())
    }

    pub fn reference_key(&self) -> Option<(usize, i64, i64)> {
        if self.is_notext() {
            return None;
        }
        Some((
            Rc::as_ptr(self.obj.as_ref()?) as usize,
            self.start,
            self.length,
        ))
    }

    pub fn references_same_frame(&self, other: &Self) -> bool {
        match (self.reference_key(), other.reference_key()) {
            (None, None) => true,
            (Some(left), Some(right)) => left == right,
            _ => false,
        }
    }

    pub fn concat(&self, other: &Self) -> Self {
        let combined = format!("{}{}", self.content(), other.content());
        Self::from_mutable(&combined)
    }

    pub fn deedit_getint(&mut self) -> Result<i64, String> {
        let content = self.content();
        let (digits, consumed) = parse_integer_item(&content)?;
        let value = digits
            .parse::<i64>()
            .map_err(|_| "integer out of range".to_string())?;
        self.pos = consumed as i64 + 1;
        Ok(value)
    }

    pub fn deedit_getreal(&mut self) -> Result<f64, String> {
        self.deedit_getreal_with('.', '&')
    }

    /// De-edit a real item using the ENVIRONMENT decimal-mark and lowten characters.
    pub fn deedit_getreal_with(&mut self, decimal_mark: char, lowten: char) -> Result<f64, String> {
        let content = self.content();
        let (number, consumed) = parse_real_item_with(&content, decimal_mark, lowten)?;
        self.pos = consumed as i64 + 1;
        Ok(number)
    }

    pub fn deedit_getfrac(&mut self) -> Result<i64, String> {
        self.deedit_getfrac_with('.')
    }

    pub fn deedit_getfrac_with(&mut self, decimal_mark: char) -> Result<i64, String> {
        let content = self.content();
        let (value, consumed) = parse_grouped_item_with(&content, decimal_mark)?;
        self.pos = consumed as i64 + 1;
        Ok(value)
    }

    pub fn edit_putint(&mut self, value: i64) -> Result<(), String> {
        self.edit_numeric(&format!("{value}"))
    }

    pub fn edit_putfix(&mut self, value: f64, places: i64) -> Result<(), String> {
        self.edit_putfix_with(value, places, '.')
    }

    pub fn edit_putfix_with(
        &mut self,
        value: f64,
        places: i64,
        decimal_mark: char,
    ) -> Result<(), String> {
        if places < 0 {
            return Err("putfix: n < 0".into());
        }
        let value = if value == 0.0 {
            0.0_f64.copysign(1.0)
        } else {
            value
        };
        let formatted = if places == 0 {
            format!("{}", value.round() as i64)
        } else {
            format!("{:.*}", places as usize, value).replace('.', &decimal_mark.to_string())
        };
        self.edit_numeric(&formatted)
    }

    pub fn edit_putreal(&mut self, value: f64, n: i64) -> Result<(), String> {
        self.edit_putreal_with(value, n, '.', '&')
    }

    pub fn edit_putreal_with(
        &mut self,
        value: f64,
        n: i64,
        decimal_mark: char,
        lowten: char,
    ) -> Result<(), String> {
        if n < 0 {
            return Err("putreal: n < 0".into());
        }
        let value = if value == 0.0 {
            0.0_f64.copysign(1.0)
        } else {
            value
        };
        let digits = if n <= 1 { 0 } else { (n - 1) as usize };
        let raw = if n == 0 {
            format!("{value:.6e}")
        } else {
            format!("{value:.digits$e}")
        };
        let formatted = format_scientific_item(&raw, decimal_mark, lowten, 2);
        self.edit_numeric(&formatted)
    }

    /// Like [`edit_putreal_with`] but with a LONG REAL-sized exponent field.
    pub fn edit_putreal_long_with(
        &mut self,
        value: f64,
        n: i64,
        decimal_mark: char,
        lowten: char,
    ) -> Result<(), String> {
        if n < 0 {
            return Err("putreal: n < 0".into());
        }
        let value = if value == 0.0 {
            0.0_f64.copysign(1.0)
        } else {
            value
        };
        let digits = if n <= 1 { 0 } else { (n - 1) as usize };
        let raw = if n == 0 {
            format!("{value:.6e}")
        } else {
            format!("{value:.digits$e}")
        };
        let formatted = format_scientific_item(&raw, decimal_mark, lowten, 3);
        self.edit_numeric(&formatted)
    }

    pub fn edit_putfrac(&mut self, value: i64, places: i64) -> Result<(), String> {
        self.edit_putfrac_with(value, places, '.')
    }

    pub fn edit_putfrac_with(
        &mut self,
        value: i64,
        places: i64,
        decimal_mark: char,
    ) -> Result<(), String> {
        // Standard §8.7: n<=0 → no decimal mark. Extreme negative n is treated
        // like n=0 for the digit string of `value` (DosTestBatch simtst18).
        let places = if places < 0 { 0 } else { places };
        let negative = value < 0;
        let abs = value.unsigned_abs();
        let scale = 10_u64.pow(places as u32);
        let whole = if places == 0 { abs } else { abs / scale };
        let frac = if places == 0 { 0 } else { abs % scale };
        let mut body = String::new();
        if places > 0 && whole == 0 {
            // GROUPED-ITEM may start with DECIMAL-MARK (no leading zero).
            body.push(decimal_mark);
            body.push_str(&group_fractional_part(&format!(
                "{frac:0places$}",
                places = places as usize
            )));
        } else {
            body.push_str(&group_integer_part(&whole.to_string()));
            if places > 0 {
                body.push(decimal_mark);
                body.push_str(&group_fractional_part(&format!(
                    "{frac:0places$}",
                    places = places as usize
                )));
            }
        }
        let formatted = if negative { format!("-{body}") } else { body };
        self.edit_numeric(&formatted)
    }

    fn edit_numeric(&mut self, item: &str) -> Result<(), String> {
        if self.is_notext() || self.constant() {
            return Err("edit on notext or constant text".into());
        }
        if item.len() as i64 > self.length {
            self.write_content(&"*".repeat(self.length as usize))?;
        } else {
            let padded = format!("{:>width$}", item, width = self.length as usize);
            self.write_content(&padded)?;
        }
        self.pos = self.length + 1;
        Ok(())
    }
}

/// Normalize Rust/`printf` scientific notation into Simula `d.ddd&±ee` form
/// with a signed, zero-padded exponent of `exp_digits` width.
fn format_scientific_item(
    raw: &str,
    decimal_mark: char,
    lowten: char,
    exp_digits: usize,
) -> String {
    let lower = raw.replace('E', "e");
    let Some((mant, exp)) = lower.split_once('e') else {
        return raw
            .replace('e', &lowten.to_string())
            .replace('E', &lowten.to_string())
            .replace('.', &decimal_mark.to_string());
    };
    let exp_val: i32 = exp.parse().unwrap_or(0);
    let mant = mant.replace('.', &decimal_mark.to_string());
    if exp_val >= 0 {
        format!("{mant}{lowten}+{exp_val:0width$}", width = exp_digits)
    } else {
        format!(
            "{mant}{lowten}-{exp_abs:0width$}",
            exp_abs = -exp_val,
            width = exp_digits
        )
    }
}

fn group_integer_part(integer: &str) -> String {
    let chars: Vec<char> = integer.chars().collect();
    if chars.len() <= 3 {
        return integer.to_string();
    }
    let mut parts = Vec::new();
    let first_len = match chars.len() % 3 {
        0 => 3,
        remainder => remainder,
    };
    parts.push(chars[..first_len].iter().collect::<String>());
    let mut index = first_len;
    while index < chars.len() {
        parts.push(chars[index..index + 3].iter().collect());
        index += 3;
    }
    parts.join(" ")
}

/// Group fractional digits from the left into threes (Simula `putfrac`).
fn group_fractional_part(fraction: &str) -> String {
    let chars: Vec<char> = fraction.chars().collect();
    if chars.len() <= 3 {
        return fraction.to_string();
    }
    let mut parts = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        let end = (index + 3).min(chars.len());
        parts.push(chars[index..end].iter().collect::<String>());
        index = end;
    }
    parts.join(" ")
}

/// INTEGER-ITEM: `SIGN-PART DIGITS` where `SIGN-PART = BLANKS [SIGN] BLANKS`.
fn parse_integer_item(input: &str) -> Result<(String, usize), String> {
    let chars: Vec<char> = input.chars().collect();
    let mut index = 0;
    while index < chars.len() && matches!(chars[index], ' ' | '\t') {
        index += 1;
    }
    let mut sign = '\0';
    if index < chars.len() && matches!(chars[index], '+' | '-') {
        sign = chars[index];
        index += 1;
    }
    while index < chars.len() && matches!(chars[index], ' ' | '\t') {
        index += 1;
    }
    let digits_start = index;
    while index < chars.len() && chars[index].is_ascii_digit() {
        index += 1;
    }
    if index == digits_start {
        return Err("no numeric item".into());
    }
    let mut token = String::new();
    if sign != '\0' {
        token.push(sign);
    }
    token.extend(chars[digits_start..index].iter());
    let consumed_bytes: usize = chars[..index].iter().map(|c| c.len_utf8()).sum();
    Ok((token, consumed_bytes))
}

fn parse_real_item_with(
    input: &str,
    decimal_mark: char,
    lowten: char,
) -> Result<(f64, usize), String> {
    let chars: Vec<char> = input.chars().collect();
    let mut index = 0;
    let skip_blanks = |index: &mut usize| {
        while *index < chars.len() && matches!(chars[*index], ' ' | '\t') {
            *index += 1;
        }
    };
    skip_blanks(&mut index);
    let mut signed = String::new();
    if index < chars.len() && matches!(chars[index], '+' | '-') {
        signed.push(chars[index]);
        index += 1;
    }
    skip_blanks(&mut index);
    let item_body_start = index;
    let mut saw_digit = false;
    while index < chars.len() && chars[index].is_ascii_digit() {
        saw_digit = true;
        index += 1;
    }
    if index < chars.len()
        && (chars[index] == decimal_mark || chars[index] == '.' || chars[index] == ',')
    {
        index += 1;
        while index < chars.len() && chars[index].is_ascii_digit() {
            saw_digit = true;
            index += 1;
        }
    }
    if index < chars.len()
        && (chars[index] == lowten
            || chars[index] == '&'
            || chars[index] == 'e'
            || chars[index] == 'E')
    {
        index += 1;
        skip_blanks(&mut index);
        if index < chars.len() && matches!(chars[index], '+' | '-') {
            index += 1;
        }
        skip_blanks(&mut index);
        let exp_start = index;
        while index < chars.len() && chars[index].is_ascii_digit() {
            saw_digit = true;
            index += 1;
        }
        if index == exp_start {
            return Err("no numeric item".into());
        }
    }
    if !saw_digit {
        return Err("no numeric item".into());
    }
    let consumed_bytes: usize = chars[..index].iter().map(|c| c.len_utf8()).sum();
    let body: String = chars[item_body_start..index].iter().collect();
    let mut token = format!("{signed}{body}");
    if decimal_mark != '.' {
        token = token.replace(decimal_mark, ".");
    }
    token = token.replace(',', ".");
    if lowten != 'e' && lowten != 'E' {
        token = token.replace(lowten, "e");
    }
    token = token.replace('&', "e");
    // Collapse blanks inserted by SIGN-PART inside the exponent / mantissa.
    token = token.split_whitespace().collect::<String>();
    token
        .parse::<f64>()
        .map(|value| (value, consumed_bytes))
        .map_err(|_| "real out of range".to_string())
}

/// GROUPED-ITEM (§8.5): `SIGN-PART GROUPS [DECIMAL-MARK GROUPS]`
/// or `SIGN-PART DECIMAL-MARK GROUPS`, with `GROUPS = DIGITS { BLANKS DIGITS }`.
fn parse_grouped_item_with(input: &str, decimal_mark: char) -> Result<(i64, usize), String> {
    let chars: Vec<char> = input.chars().collect();
    let mut index = 0;
    let is_blank = |ch: char| matches!(ch, ' ' | '\t');
    let is_mark = |ch: char| ch == decimal_mark || ch == '.' || ch == ',';
    while index < chars.len() && is_blank(chars[index]) {
        index += 1;
    }
    let mut negative = false;
    if index < chars.len() && matches!(chars[index], '+' | '-') {
        negative = chars[index] == '-';
        index += 1;
    }
    while index < chars.len() && is_blank(chars[index]) {
        index += 1;
    }

    let parse_groups = |index: &mut usize| -> bool {
        if *index >= chars.len() || !chars[*index].is_ascii_digit() {
            return false;
        }
        while *index < chars.len() && chars[*index].is_ascii_digit() {
            *index += 1;
        }
        loop {
            let mut look = *index;
            while look < chars.len() && is_blank(chars[look]) {
                look += 1;
            }
            if look > *index && look < chars.len() && chars[look].is_ascii_digit() {
                *index = look;
                while *index < chars.len() && chars[*index].is_ascii_digit() {
                    *index += 1;
                }
            } else {
                break;
            }
        }
        true
    };

    let start = index;
    if index < chars.len() && is_mark(chars[index]) {
        index += 1;
        if !parse_groups(&mut index) {
            return Err("no numeric item".into());
        }
    } else {
        if !parse_groups(&mut index) {
            return Err("no numeric item".into());
        }
        // Optional `[ DECIMAL-MARK GROUPS ]` — only if both parts match.
        if index < chars.len() && is_mark(chars[index]) {
            let before_mark = index;
            let mut after = index + 1;
            if parse_groups(&mut after) {
                index = after;
            } else {
                index = before_mark;
            }
        }
    }

    let digits: String = chars[start..index]
        .iter()
        .copied()
        .filter(|ch| ch.is_ascii_digit())
        .collect();
    if digits.is_empty() {
        return Err("no numeric item".into());
    }
    let mut value = digits
        .parse::<i64>()
        .map_err(|_| "grouped item out of range".to_string())?;
    if negative {
        value = -value;
    }
    let consumed_bytes: usize = chars[..index].iter().map(|c| c.len_utf8()).sum();
    Ok((value, consumed_bytes))
}

impl PartialEq for TextFrame {
    fn eq(&self, other: &Self) -> bool {
        self.content() == other.content()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notext_has_zero_length() {
        let frame = TextFrame::notext();
        assert!(frame.is_notext());
        assert_eq!(frame.length, 0);
        assert_eq!(frame.start, 1);
        assert_eq!(frame.pos, 1);
        assert!(frame.constant());
        assert_eq!(frame.content(), "");
    }

    #[test]
    fn literal_frame_is_constant() {
        let frame = TextFrame::from_literal("ABC", true);
        assert!(frame.constant());
        assert_eq!(frame.content(), "ABC");
    }

    #[test]
    fn main_frame_spans_full_buffer() {
        let frame = TextFrame::from_literal("ABC", true);
        let sub = frame.subframe(2, 1).unwrap();
        assert_eq!(sub.main_frame().content(), "ABC");
        assert!(sub.main_frame().references_same_frame(&frame.main_frame()));
    }

    #[test]
    fn subframe_nested_equivalence() {
        let frame = TextFrame::from_literal("hello", false);
        let outer = frame.subframe(2, 3).unwrap();
        let inner = outer.subframe(2, 1).unwrap();
        let direct = frame.subframe(3, 1).unwrap();
        assert_eq!(inner.content(), direct.content());
        assert_eq!(inner.start, direct.start);
    }

    #[test]
    fn strip_trims_trailing_blanks() {
        let mut frame = TextFrame::from_mutable("abc   ");
        frame.pos = 4;
        let stripped = frame.strip();
        assert_eq!(stripped.content(), "abc");
        assert_eq!(frame.pos, 4, "strip must not alter source pos");
    }

    #[test]
    fn getchar_and_putchar_update_pos() {
        let mut frame = TextFrame::from_mutable("ab");
        assert_eq!(frame.getchar().unwrap(), 'a');
        assert_eq!(frame.pos, 2);
        frame.putchar('X').unwrap();
        assert_eq!(frame.content(), "aX");
        assert_eq!(frame.pos, 3);
    }

    #[test]
    fn blanks_zero_is_notext() {
        assert!(TextFrame::blanks(0).unwrap().is_notext());
    }

    #[test]
    fn copy_duplicates_value() {
        let source = TextFrame::from_literal("hi", true);
        let copied = TextFrame::copy(&source);
        assert_eq!(copied.content(), "hi");
        assert!(!copied.constant());
        assert!(!copied.references_same_frame(&source));
    }

    #[test]
    fn deedit_and_edit_respect_decimal_mark_and_lowten() {
        let mut frame = TextFrame::from_mutable("  12,5*3  ");
        let value = frame.deedit_getreal_with(',', '*').unwrap();
        assert!((value - 12500.0).abs() < 1e-9);

        let mut out = TextFrame::blanks(16).unwrap();
        out.edit_putreal_with(12.5, 2, ',', '*').unwrap();
        let text = out.content();
        assert!(
            text.contains(',') && text.contains('*'),
            "expected continental decimal mark and custom lowten in {text:?}"
        );
        assert!(!text.contains('.'), "unexpected ascii decimal in {text:?}");
        assert!(
            !text.contains('e') && !text.contains('E') && !text.contains('&'),
            "{text:?}"
        );
    }

    #[test]
    fn getint_allows_blanks_in_sign_part() {
        let mut frame = TextFrame::from_mutable(" + 24 2");
        assert_eq!(frame.deedit_getint().unwrap(), 24);
        assert_eq!(frame.pos, 6);
    }

    #[test]
    fn getfrac_follows_grouped_item_grammar() {
        let mut frame = TextFrame::from_mutable("12 3 45 . 67");
        assert_eq!(frame.deedit_getfrac().unwrap(), 12345);
        assert_eq!(frame.pos, 8);
        frame.putchar('0').unwrap();
        assert_eq!(frame.deedit_getfrac().unwrap(), 123450);
    }

    #[test]
    fn putfrac_groups_fraction_and_omits_leading_zero() {
        let mut frame = TextFrame::blanks(20).unwrap();
        frame.edit_putfrac(1234567, 7).unwrap();
        assert_eq!(frame.content(), "          .123 456 7");
        frame.edit_putfrac(1234567, 0).unwrap();
        assert_eq!(frame.content(), "           1 234 567");
        frame.edit_putfrac(-1234567, 7).unwrap();
        assert_eq!(frame.content(), "         -.123 456 7");
    }

    #[test]
    fn putreal_rounds_12_5_ties_to_even() {
        let mut frame = TextFrame::blanks(16).unwrap();
        frame.edit_putreal(12.5, 2).unwrap();
        assert_eq!(frame.content(), "         1.2&+01");
    }

    #[test]
    fn putreal_uses_signed_zero_padded_exponent() {
        let mut frame = TextFrame::blanks(30).unwrap();
        frame.edit_putreal(123456.0, 7).unwrap();
        assert_eq!(frame.content(), "                  1.234560&+05");
        frame
            .edit_putreal_long_with(12345678912345678.0, 16, '.', '&')
            .unwrap();
        assert_eq!(frame.content(), "        1.234567891234568&+016");
    }
}
