use anyhow::{Result, bail};

use crate::app::App;
use crate::cli::ReadOptions;
use crate::commands::resolve_session;
use crate::index;
use crate::render::{self, Detail, Position, Within};
use crate::view::{Earlier, ReadMode, ReadView};

pub fn run(app: &App, options: &ReadOptions) -> Result<ReadView> {
    let row = resolve_session(&app.store, &app.filter, &options.session)?;
    let session = index::load_session(&app.store, &row.path, &row.session_id)?;
    let count = session.turns.len();
    let last_turn = count.saturating_sub(1);
    let requested = options.turn.or(options.from.map(|from| from.turn));
    if let Some(turn) = requested
        && turn > last_turn
    {
        bail!("turn {turn} does not exist, the session has turns 0–{last_turn}");
    }
    if let Some(from) = options.from
        && matches!(from.within, Within::Block(_))
        && !options.deep
    {
        bail!("{from} is a position in a --deep page; add --deep, or name a part as in {}.0", from.turn);
    }
    let budget = options.budget.unwrap_or(app.config.budget);
    let header = render::header(&session, row.archived);

    if options.outline {
        let page = render::outline(&session, requested.unwrap_or(0), budget);
        return Ok(ReadView { scope: app.filter.scope.clone(), session: row, header, page, last_turn, mode: ReadMode::Outline });
    }

    let context = options.context.unwrap_or(app.config.context);
    let (from, end) = read_range(options.turn, options.from, context, count);
    let detail = if options.deep { Detail::Deep } else { Detail::Default };
    let page = render::page(&session, from, end, detail, budget);
    let later = (end < count).then_some(end);
    let earlier = (from.turn > 0 && options.turn.is_some()).then(|| Earlier { turn: from.turn.saturating_sub(context + 1), context });
    Ok(ReadView {
        scope: app.filter.scope.clone(),
        session: row,
        header,
        page,
        last_turn,
        mode: ReadMode::Content { deep: options.deep, later, earlier },
    })
}

fn read_range(turn: Option<usize>, from: Option<Position>, context: usize, count: usize) -> (Position, usize) {
    match (turn, from) {
        (Some(turn), _) => (Position::start(turn.saturating_sub(context)), (turn + context + 1).min(count)),
        (None, Some(from)) => (from, count),
        (None, None) => (Position::start(0), count),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn at(turn: usize, part: usize) -> Position {
        Position::part(turn, part)
    }

    #[test]
    fn without_options_the_whole_session_is_read() {
        assert_eq!(read_range(None, None, 2, 10), (at(0, 0), 10));
    }

    #[test]
    fn from_reads_to_the_end() {
        assert_eq!(read_range(None, Some(at(4, 2)), 2, 10), (at(4, 2), 10));
    }

    #[test]
    fn turn_shows_context_either_side() {
        assert_eq!(read_range(Some(5), None, 2, 10), (at(3, 0), 8));
        assert_eq!(read_range(Some(5), None, 0, 10), (at(5, 0), 6));
    }

    #[test]
    fn turn_context_is_clamped_to_the_session() {
        assert_eq!(read_range(Some(1), None, 3, 10), (at(0, 0), 5));
        assert_eq!(read_range(Some(9), None, 3, 10), (at(6, 0), 10));
        assert_eq!(read_range(Some(0), None, 100, 1), (at(0, 0), 1));
    }

    #[test]
    fn turn_wins_over_from() {
        assert_eq!(read_range(Some(5), Some(at(1, 1)), 1, 10), (at(4, 0), 7));
    }
}
