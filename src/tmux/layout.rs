//! Recursive tmux window-layout parsing and minimum-width policy.
//!
//! A tmux window is tiled by a recursive split tree. Pane leaves may be grouped
//! side by side, where their widths and intervening separators add together, or
//! stacked, where they share width and the widest child determines the group's
//! minimum. Retaining this topology matters because a flat list of pane
//! rectangles cannot describe every nested layout's resize constraints.

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq)]
/// Parsed recursive `#{window_layout}` topology used to calculate minimum pane geometry.
pub struct TmuxWindowLayout {
    root: TmuxLayoutCell,
}

impl TmuxWindowLayout {
    /// Return the minimum cell width for this recursive layout.
    ///
    /// Each pane needs one content cell. Side-by-side children add their widths
    /// and one separator per boundary, while stacked children use the largest
    /// child width. Excluding a pane models the content layout that remains when
    /// an existing sidebar is removed from the tree before recalculating its size.
    pub fn minimum_width(&self, excluded_pane_id: Option<&str>) -> u16 {
        let excluded_pane_id = excluded_pane_id.map(|pane_id| pane_id.trim_start_matches('%'));
        self.root.minimum_width(excluded_pane_id).unwrap_or(1)
    }

    /// Parse either tmux 3.8's JSON layout or the legacy checksum-prefixed layout.
    pub(super) fn parse(value: &str) -> Result<Self> {
        if value.starts_with('{') {
            Self::parse_json(value)
        } else {
            Self::parse_legacy(value)
        }
    }

    /// Parse the versioned JSON description that tmux 3.8 and later emit.
    fn parse_json(value: &str) -> Result<Self> {
        let layout: JsonLayout = serde_json::from_str(value)
            .with_context(|| format!("invalid tmux window layout {value:?}"))?;
        if layout.version != JSON_LAYOUT_VERSION {
            bail!(
                "unsupported tmux window layout version {} in {value:?}",
                layout.version
            );
        }
        Ok(Self {
            root: layout.root.into(),
        })
    }

    /// Parse the `checksum,WxH,X,Y...` description that tmux emitted before 3.8.
    fn parse_legacy(value: &str) -> Result<Self> {
        let (_, body) = value
            .split_once(',')
            .with_context(|| format!("invalid tmux window layout {value:?}: missing checksum"))?;
        let mut parser = TmuxLayoutParser::new(body, value);
        let root = parser.parse_cell()?;
        if !parser.is_finished() {
            bail!(
                "invalid tmux window layout {value:?}: trailing input at byte {}",
                parser.offset
            );
        }
        Ok(Self { root })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TmuxLayoutCell {
    /// A physical pane leaf, identified without tmux's leading `%` sigil.
    Pane(String),
    /// Children arranged side by side, consuming the sum of their widths and separators.
    Horizontal(Vec<Self>),
    /// Children stacked top to bottom, sharing the width of their widest child.
    Vertical(Vec<Self>),
}

impl TmuxLayoutCell {
    fn minimum_width(&self, excluded_pane_id: Option<&str>) -> Option<u16> {
        match self {
            Self::Pane(pane_id) => (excluded_pane_id != Some(pane_id.as_str())).then_some(1),
            Self::Horizontal(children) => {
                let mut width = 0u16;
                let mut count = 0u16;
                for child_width in children
                    .iter()
                    .filter_map(|child| child.minimum_width(excluded_pane_id))
                {
                    if count > 0 {
                        width = width.saturating_add(1);
                    }
                    width = width.saturating_add(child_width);
                    count = count.saturating_add(1);
                }
                (count > 0).then_some(width)
            }
            Self::Vertical(children) => children
                .iter()
                .filter_map(|child| child.minimum_width(excluded_pane_id))
                .max(),
        }
    }
}

const JSON_LAYOUT_VERSION: u32 = 2;

/// Top-level tmux JSON layout envelope; fields other than the tree are ignored.
#[derive(Deserialize)]
struct JsonLayout {
    #[serde(rename = "V")]
    version: u32,
    #[serde(rename = "L")]
    root: JsonLayoutCell,
}

/// One JSON layout node, tagged by `t`: `p` pane, `h` side by side, `v` stacked.
#[derive(Deserialize)]
#[serde(tag = "t")]
enum JsonLayoutCell {
    #[serde(rename = "p")]
    Pane {
        #[serde(rename = "I")]
        pane_id: String,
    },
    #[serde(rename = "h")]
    Horizontal { c: Vec<Self> },
    #[serde(rename = "v")]
    Vertical { c: Vec<Self> },
}

impl From<JsonLayoutCell> for TmuxLayoutCell {
    fn from(cell: JsonLayoutCell) -> Self {
        match cell {
            JsonLayoutCell::Pane { pane_id } => {
                Self::Pane(pane_id.trim_start_matches('%').to_owned())
            }
            JsonLayoutCell::Horizontal { c } => {
                Self::Horizontal(c.into_iter().map(Into::into).collect())
            }
            JsonLayoutCell::Vertical { c } => {
                Self::Vertical(c.into_iter().map(Into::into).collect())
            }
        }
    }
}

struct TmuxLayoutParser<'a> {
    body: &'a str,
    original: &'a str,
    offset: usize,
}

impl<'a> TmuxLayoutParser<'a> {
    fn new(body: &'a str, original: &'a str) -> Self {
        Self {
            body,
            original,
            offset: 0,
        }
    }

    fn parse_cell(&mut self) -> Result<TmuxLayoutCell> {
        self.parse_number_before(b'x', "width")?;
        self.parse_number_before(b',', "height")?;
        self.parse_number_before(b',', "x offset")?;
        self.parse_number("y offset")?;

        match self.peek() {
            Some(b',') => {
                self.offset += 1;
                let pane_id = self.parse_number("pane id")?;
                Ok(TmuxLayoutCell::Pane(pane_id.to_owned()))
            }
            Some(b'{') => self
                .parse_children(b'{', b'}')
                .map(TmuxLayoutCell::Horizontal),
            Some(b'[') => self
                .parse_children(b'[', b']')
                .map(TmuxLayoutCell::Vertical),
            _ => bail!(
                "invalid tmux window layout {:?}: expected pane or child layout at byte {}",
                self.original,
                self.offset
            ),
        }
    }

    fn parse_children(&mut self, open: u8, close: u8) -> Result<Vec<TmuxLayoutCell>> {
        self.expect(open)?;
        let mut children = Vec::new();
        loop {
            children.push(self.parse_cell()?);
            match self.peek() {
                Some(b',') => self.offset += 1,
                Some(value) if value == close => {
                    self.offset += 1;
                    return Ok(children);
                }
                _ => {
                    bail!(
                        "invalid tmux window layout {:?}: expected separator or {:?} at byte {}",
                        self.original,
                        char::from(close),
                        self.offset
                    )
                }
            }
        }
    }

    fn parse_number_before(&mut self, delimiter: u8, field: &str) -> Result<()> {
        self.parse_number(field)?;
        self.expect(delimiter)
    }

    fn parse_number(&mut self, field: &str) -> Result<&'a str> {
        let start = self.offset;
        while self.peek().is_some_and(|value| value.is_ascii_digit()) {
            self.offset += 1;
        }
        if start == self.offset {
            bail!(
                "invalid tmux window layout {:?}: expected {field} at byte {}",
                self.original,
                self.offset
            );
        }
        Ok(&self.body[start..self.offset])
    }

    fn expect(&mut self, expected: u8) -> Result<()> {
        if self.peek() != Some(expected) {
            bail!(
                "invalid tmux window layout {:?}: expected {:?} at byte {}",
                self.original,
                char::from(expected),
                self.offset
            );
        }
        self.offset += 1;
        Ok(())
    }

    fn peek(&self) -> Option<u8> {
        self.body.as_bytes().get(self.offset).copied()
    }

    fn is_finished(&self) -> bool {
        self.offset == self.body.len()
    }
}

#[cfg(test)]
pub(super) fn test_window_layout(pane_ids: &[&str]) -> TmuxWindowLayout {
    let panes = pane_ids
        .iter()
        .map(|pane_id| TmuxLayoutCell::Pane(pane_id.trim_start_matches('%').to_owned()))
        .collect::<Vec<_>>();
    let root = match panes.as_slice() {
        [pane] => pane.clone(),
        _ => TmuxLayoutCell::Horizontal(panes),
    };
    TmuxWindowLayout { root }
}

#[cfg(test)]
mod tests {
    use anyhow::Result;

    use super::TmuxWindowLayout;

    #[test]
    fn window_layout_calculates_nested_minimum_width() -> Result<()> {
        let horizontal = TmuxWindowLayout::parse("89f5,80x24,0,0{39x24,0,0,0,40x24,40,0,1}")?;
        let vertical = TmuxWindowLayout::parse("1247,80x24,0,0[80x11,0,0,0,80x12,0,12,1]")?;
        let staggered = TmuxWindowLayout::parse(
            "0000,20x20,0,0{9x20,0,0[9x9,0,0{4x9,0,0,0,4x9,5,0,1},9x10,0,10,2],10x20,10,0[10x9,10,0,3,10x10,10,10{4x10,10,10,4,5x10,15,10,5}]}",
        )?;

        assert_eq!(horizontal.minimum_width(None), 3);
        assert_eq!(vertical.minimum_width(None), 1);
        assert_eq!(staggered.minimum_width(None), 7);
        Ok(())
    }

    #[test]
    fn window_layout_excludes_sidebar_pane_from_minimum_width() -> Result<()> {
        let layout = TmuxWindowLayout::parse(
            "0000,20x20,0,0{12x20,0,0,9,7x20,13,0{3x20,13,0,1,3x20,17,0,2}}",
        )?;

        assert_eq!(layout.minimum_width(None), 5);
        assert_eq!(layout.minimum_width(Some("%9")), 3);
        Ok(())
    }

    #[test]
    fn json_window_layout_matches_legacy_minimum_width() -> Result<()> {
        let json = TmuxWindowLayout::parse(
            r#"{"V":2,"L":{"t":"h","w":20,"h":20,"x":0,"y":0,"c":[{"t":"p","w":12,"h":20,"x":0,"y":0,"i":0,"I":"%9"},{"t":"v","w":7,"h":20,"x":13,"y":0,"c":[{"t":"h","w":7,"h":10,"x":13,"y":0,"c":[{"t":"p","w":3,"h":10,"x":13,"y":0,"i":1,"I":"%1"},{"t":"p","w":3,"h":10,"x":17,"y":0,"a":true,"i":2,"I":"%2"}]},{"t":"p","w":7,"h":9,"x":13,"y":11,"l":0,"i":3,"I":"%3"}]}]}}"#,
        )?;
        let legacy = TmuxWindowLayout::parse(
            "0000,20x20,0,0{12x20,0,0,9,7x20,13,0[7x10,13,0{3x10,13,0,1,3x10,17,0,2},7x9,13,11,3]}",
        )?;

        assert_eq!(json, legacy);
        assert_eq!(json.minimum_width(None), 5);
        assert_eq!(json.minimum_width(Some("%9")), 3);
        Ok(())
    }

    #[test]
    fn json_window_layout_rejects_unknown_version() {
        let error = TmuxWindowLayout::parse(
            r#"{"V":3,"L":{"t":"p","w":80,"h":24,"x":0,"y":0,"i":0,"I":"%0"}}"#,
        )
        .expect_err("unknown layout version should fail parsing");

        assert!(
            error
                .to_string()
                .contains("unsupported tmux window layout version 3")
        );
    }

    #[test]
    fn malformed_window_layout_reports_context() {
        let error = TmuxWindowLayout::parse("invalid")
            .expect_err("malformed window layout should fail parsing");

        assert!(error.to_string().contains("tmux window layout"));
    }
}
