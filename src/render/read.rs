use std::fmt::{self, Display, Formatter};

use crate::render::{Page, TurnRange, invocation};
use crate::view::{ReadMode, ReadView};

pub fn read(view: &ReadView) -> String {
    view.to_string()
}

impl Display for ReadView {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let command = invocation(&self.scope);
        let id = self.session.short_id();
        let page = &self.page;
        let shown = Shown { page, last: self.last_turn };
        f.write_str(&self.header)?;
        match &self.mode {
            ReadMode::Outline => {
                write!(f, "\n{}\n{shown}", page.text)?;
                if let Some(next) = page.next {
                    write!(f, " More: `{command} read {id} --outline --from {}`", next.turn)?;
                }
                writeln!(f, " Read a turn: `{command} read {id} --from N`")
            }
            ReadMode::Content { deep, later, earlier } => {
                let deep_flag = if *deep { " --deep" } else { "" };
                write!(f, "{}\n---\n\n{shown}", page.text)?;
                if let Some(next) = page.next {
                    write!(f, " Continue: `{command} read {id} --from {next}{deep_flag}` (`--budget N` for a longer page)")?;
                } else if let Some(later) = later {
                    write!(f, " Later: `{command} read {id} --from {later}{deep_flag}`")?;
                }
                if let Some(earlier) = earlier {
                    write!(f, " Earlier: `{command} read {id} --turn {} --context {}{deep_flag}`", earlier.turn, earlier.context)?;
                }
                if !deep {
                    f.write_str(" `--deep` adds tool calls and thinking.")?;
                }
                f.write_str(" `--outline` lists all turns.\n")
            }
        }
    }
}

struct Shown<'a> {
    page: &'a Page,
    last: usize,
}

impl Display for Shown<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let page = TurnRange { first: self.page.first, last: self.page.last };
        write!(f, "*Turns {page} of {}.*", TurnRange::upto(self.last))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discover::Scope;
    use crate::render::Position;
    use crate::store::SessionRow;
    use crate::test_support::timestamp;
    use crate::transcript::SessionMeta;
    use crate::view::Earlier;

    fn view(scope: Scope, page: Page, mode: ReadMode) -> ReadView {
        let session = SessionRow {
            path: "/projects/-p/aaaa1111.jsonl".to_owned(),
            session_id: "aaaa1111-0000-4000-8000-000000000001".to_owned(),
            meta: SessionMeta {
                title: "Fix token refresh".to_owned(),
                cwd: Some("/p".to_owned()),
                first: timestamp("2026-03-01T09:00:00.000Z"),
                last: timestamp("2026-03-01T10:00:00.000Z"),
            },
            turns: 10,
            archived: false,
        };
        ReadView { scope, session, header: "# Fix token refresh\n\n**Date:** …\n".to_owned(), page, last_turn: 9, mode }
    }

    fn page(first: usize, last: usize, next: Option<Position>) -> Page {
        Page { text: "\n---\n\n### #2 👤 User — 09:02\n\nHello\n".to_owned(), first, last, next }
    }

    fn directory() -> Scope {
        Scope::Directory { root: "/p".into() }
    }

    const fn content(deep: bool, later: Option<usize>, earlier: Option<Earlier>) -> ReadMode {
        ReadMode::Content { deep, later, earlier }
    }

    fn footer(view: &ReadView) -> String {
        read(view).lines().last().unwrap().to_owned()
    }

    #[test]
    fn a_page_ends_in_a_separator_and_the_footer() {
        insta::assert_snapshot!(read(&view(directory(), page(2, 4, None), content(false, None, None))), @r"
        # Fix token refresh

        **Date:** …

        ---

        ### #2 👤 User — 09:02

        Hello

        ---

        *Turns 2–4 of 0–9.* `--deep` adds tool calls and thinking. `--outline` lists all turns.
        ");
    }

    #[test]
    fn a_cut_page_continues_where_it_stopped() {
        let next = Some(Position::part(3, 1));
        assert_eq!(
            footer(&view(directory(), page(2, 3, next), content(false, Some(6), None))),
            "*Turns 2–3 of 0–9.* Continue: `claudegrep read aaaa1111 --from 3.1` (`--budget N` for a longer page) \
             `--deep` adds tool calls and thinking. `--outline` lists all turns."
        );
    }

    #[test]
    fn a_turn_range_points_later_and_earlier() {
        let earlier = Some(Earlier { turn: 0, context: 1 });
        assert_eq!(
            footer(&view(directory(), page(2, 4, None), content(false, Some(5), earlier))),
            "*Turns 2–4 of 0–9.* Later: `claudegrep read aaaa1111 --from 5` Earlier: `claudegrep read aaaa1111 --turn 0 --context 1` \
             `--deep` adds tool calls and thinking. `--outline` lists all turns."
        );
    }

    #[test]
    fn deep_hints_keep_deep_and_all() {
        let earlier = Some(Earlier { turn: 1, context: 2 });
        let next = Some(Position::block(4, 0));
        assert_eq!(
            footer(&view(Scope::All, page(4, 4, next), content(true, Some(7), earlier))),
            "*Turns 4–4 of 0–9.* Continue: `claudegrep --all read aaaa1111 --from 4:0 --deep` (`--budget N` for a longer page) \
             Earlier: `claudegrep --all read aaaa1111 --turn 1 --context 2 --deep` `--outline` lists all turns."
        );
    }

    #[test]
    fn an_outline_follows_the_header_and_offers_more() {
        let outline = Page {
            text: "- #0 👤 09:00 · Hello\n- #1 🤖 09:01 · 1 message · 5 chars · 0 tool calls → Hi\n".to_owned(),
            first: 0,
            last: 1,
            next: None,
        };
        insta::assert_snapshot!(read(&view(directory(), outline, ReadMode::Outline)), @r"
        # Fix token refresh

        **Date:** …

        - #0 👤 09:00 · Hello
        - #1 🤖 09:01 · 1 message · 5 chars · 0 tool calls → Hi

        *Turns 0–1 of 0–9.* Read a turn: `claudegrep read aaaa1111 --from N`
        ");
        let cut = Page { text: String::new(), first: 0, last: 1, next: Some(Position::start(2)) };
        assert_eq!(
            footer(&view(Scope::All, cut, ReadMode::Outline)),
            "*Turns 0–1 of 0–9.* More: `claudegrep --all read aaaa1111 --outline --from 2` Read a turn: `claudegrep --all read aaaa1111 --from N`"
        );
    }
}
