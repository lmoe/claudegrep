use crate::render::Position;
use crate::text::char_len;

pub struct Page {
    pub text: String,
    pub first: usize,
    pub last: usize,
    pub next: Option<Position>,
}

/// Builds a page of at most `budget` characters, 0 meaning no limit. The first push always fits, so a page is never
/// empty and paging always moves on, even past a block larger than the budget.
pub struct Pager {
    budget: usize,
    page: Page,
    used: usize,
}

impl Pager {
    pub const fn new(first: usize, budget: usize) -> Self {
        Self { budget, page: Page { text: String::new(), first, last: first, next: None }, used: 0 }
    }

    pub const fn fits(&self, cost: usize) -> bool {
        self.budget == 0 || self.used == 0 || self.used + cost <= self.budget
    }

    pub fn push(&mut self, turn: usize, text: &str) {
        self.page.text.push_str(text);
        self.page.last = turn;
        self.used += char_len(text);
    }

    pub fn finish(self, next: Option<Position>) -> Page {
        Page { next, ..self.page }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_zero_budget_has_no_limit() {
        let mut pager = Pager::new(0, 0);
        pager.push(0, &"x".repeat(10_000));
        assert!(pager.fits(usize::MAX / 2));
    }

    #[test]
    fn the_first_push_fits_whatever_it_costs() {
        let mut pager = Pager::new(3, 10);
        assert!(pager.fits(500));
        pager.push(3, &"x".repeat(500));
        assert!(!pager.fits(1));
        assert_eq!(pager.finish(None).text.len(), 500);
    }

    #[test]
    fn a_push_fits_up_to_the_budget_exactly() {
        let mut pager = Pager::new(0, 10);
        pager.push(0, "12345");
        assert!(pager.fits(5));
        assert!(!pager.fits(6));
    }

    #[test]
    fn the_budget_counts_characters_not_bytes() {
        let mut pager = Pager::new(0, 5);
        pager.push(0, "äöü");
        assert!(pager.fits(2));
        assert!(!pager.fits(3));
    }

    #[test]
    fn the_page_spans_the_turns_pushed() {
        let mut pager = Pager::new(2, 0);
        pager.push(2, "a");
        pager.push(4, "b");
        let page = pager.finish(Some(Position::part(5, 1)));
        assert_eq!((page.text.as_str(), page.first, page.last, page.next), ("ab", 2, 4, Some(Position::part(5, 1))));
    }

    #[test]
    fn an_empty_page_ends_where_it_starts() {
        let page = Pager::new(7, 100).finish(None);
        assert_eq!((page.text.as_str(), page.first, page.last, page.next), ("", 7, 7, None));
    }
}
