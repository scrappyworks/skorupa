//! One example per layout rule, through the public API only.
//!
//! The comment after a setup shows the workspaces from top to bottom, `*`
//! marking the active one and `[]` an empty one.

#![expect(
    clippy::unwrap_used,
    reason = "clippy.toml exempts test functions only; the helpers here are test code too"
)]

use skorupa_compositor_core::layout::{
    Configure, Effect, Event, Layout, MAX_APP_ID_LEN, MAX_WINDOWS, Placement, Projection, Rect,
    Size, WindowId, step,
};

const OUTPUT: Size = Size { w: 1000, h: 800 };
const STRIP: i32 = 40;
const WORK: Size = Size { w: 1000, h: 760 };
const HALF: Size = Size { w: 500, h: 760 };

fn w(id: u64) -> WindowId {
    WindowId(id)
}

fn start() -> Layout {
    Layout::new(OUTPUT, STRIP)
}

fn map(window: u64, app_id: Option<&str>, parent: Option<u64>, valid_token: bool) -> Event {
    Event::Map {
        window: w(window),
        app_id: app_id.map(str::to_owned),
        parent: parent.map(w),
        parent_same_client: true,
        valid_token,
    }
}

/// A dialog whose parent belongs to another client (xdg-foreign).
fn foreign(window: u64, parent: u64, valid_token: bool) -> Event {
    Event::Map {
        window: w(window),
        app_id: None,
        parent: Some(w(parent)),
        parent_same_client: false,
        valid_token,
    }
}

fn drawn(layout: &Layout) -> Vec<u64> {
    let windows = layout.project().windows;
    windows.iter().map(|placed| placed.window.0).collect()
}

/// An independent window the user launched.
fn open(window: u64) -> Event {
    map(window, None, None, true)
}

fn app(window: u64, app_id: &str) -> Event {
    map(window, Some(app_id), None, true)
}

fn run(layout: Layout, events: impl IntoIterator<Item = Event>) -> Layout {
    events
        .into_iter()
        .fold(layout, |layout, event| step(layout, event).0)
}

fn tiles(layout: &Layout) -> Vec<Vec<u64>> {
    let ids = |tiles: &Vec<WindowId>| tiles.iter().map(|tile| tile.0).collect();
    layout
        .workspaces()
        .iter()
        .map(|ws| ids(&ws.tiles))
        .collect()
}

fn rect(x: i32, y: i32, w: i32, h: i32) -> Rect {
    Rect { x, y, w, h }
}

fn tile(window: u64, rect: Rect) -> Placement {
    let (window, dialog) = (w(window), false);
    Placement {
        window,
        rect,
        dialog,
    }
}

fn configure(size: Size, activated: bool) -> Configure {
    Configure {
        size: Some(size),
        activated,
        fullscreen: false,
    }
}

const ALT_TAB: Event = Event::SwitcherStep {
    same_app: false,
    reverse: false,
};
const ALT_SHIFT_TAB: Event = Event::SwitcherStep {
    same_app: false,
    reverse: true,
};
const ALT_GRAVE: Event = Event::SwitcherStep {
    same_app: true,
    reverse: false,
};

fn selected(layout: &Layout) -> Option<u64> {
    layout.switcher().map(|(_, selected)| selected.0)
}

fn candidates(layout: &Layout) -> Vec<u64> {
    let (candidates, _) = layout.switcher().unwrap();
    candidates.iter().map(|window| window.0).collect()
}

fn mru(layout: &Layout) -> Vec<u64> {
    layout.mru().iter().map(|window| window.0).collect()
}

#[test]
fn req_layout_003_new_window_opens_a_workspace_directly_below_the_current_one() {
    let layout = run(start(), [open(1), open(2), Event::WorkspaceUp, open(3)]);
    // [1] [3]* [2] []
    assert_eq!(tiles(&layout), [vec![1], vec![3], vec![2], vec![]]);
    assert_eq!(layout.active(), 1);
    assert_eq!(layout.focus(), Some(w(3)));
}

#[test]
fn req_layout_006_map_without_a_token_takes_neither_focus_nor_view() {
    let before = run(start(), [open(1)]);
    let (after, effects) = step(before, map(2, None, None, false));
    assert_eq!(tiles(&after), [vec![1], vec![2], vec![]]);
    assert_eq!((after.active(), after.focus()), (0, Some(w(1))));
    assert!(!effects.iter().any(|e| matches!(e, Effect::Focus(_))));
}

#[test]
fn req_layout_006_window_mapped_while_nothing_is_focused_is_shown_and_focused() {
    // Session start: there is no focus to steal and no view to take.
    let (alone, effects) = step(start(), map(1, None, None, false));
    assert_eq!(tiles(&alone), [vec![1], vec![]]);
    assert_eq!((alone.active(), alone.focus()), (0, Some(w(1))));
    assert!(effects.contains(&Effect::Focus(Some(w(1)))));

    // The same on a workspace the user emptied and still looks at.
    let emptied = [open(1), open(2), Event::WorkspaceUp, Event::Unmap(w(1))];
    let before = run(start(), emptied);
    let after = run(before.clone(), [map(3, None, None, false)]);
    // [3]* [2] []
    assert_eq!(tiles(&after), [vec![3], vec![2], vec![]]);
    assert_eq!((after.active(), after.focus()), (0, Some(w(3))));
    assert_eq!(after.workspaces()[0].id, before.workspaces()[0].id);

    // A dialog still goes to its parent, wherever that is.
    let dialog = run(before, [map(3, None, Some(2), false)]);
    assert_eq!(tiles(&dialog), [vec![], vec![2], vec![]]);
    assert_eq!((dialog.active(), dialog.focus()), (0, None));
}

#[test]
fn req_layout_006_dialog_with_a_foreign_parent_needs_a_token() {
    let before = run(start(), [open(1)]);
    let (after, effects) = step(before.clone(), foreign(2, 1, false));
    // It is a dialog of 1 all the same, only without the keyboard.
    assert_eq!(
        (after.parent(w(2)), after.home(w(2))),
        (Some(w(1)), Some(0))
    );
    assert_eq!(after.focus(), Some(w(1)));
    assert!(!effects.iter().any(|e| matches!(e, Effect::Focus(_))));

    let granted = run(before, [foreign(2, 1, true)]);
    assert_eq!(granted.focus(), Some(w(2)));
}

#[test]
fn req_layout_006_activation_needs_a_valid_token() {
    let before = run(start(), [open(1), open(2)]);
    let refused = Event::Activate {
        window: w(1),
        valid_token: false,
    };
    assert_eq!(step(before.clone(), refused), (before.clone(), vec![]));

    let granted = Event::Activate {
        window: w(1),
        valid_token: true,
    };
    let after = run(before, [granted]);
    assert_eq!((after.active(), after.focus()), (0, Some(w(1))));
}

#[test]
fn req_layout_006_hover_focuses_only_what_is_on_screen() {
    let split = [open(1), open(2), Event::SplitWith(w(1)), open(3)];
    let layout = run(start(), split.into_iter().chain([Event::WorkspaceUp]));
    // [2 1]* [3] []
    assert_eq!(layout.focus(), Some(w(2)));
    let layout = run(layout, [Event::Hover(w(1))]);
    assert_eq!(layout.focus(), Some(w(1)));
    let layout = run(layout, [Event::Hover(w(3))]);
    assert_eq!((layout.active(), layout.focus()), (0, Some(w(1))));
    let layout = run(layout, [Event::OverviewOpen, Event::Hover(w(2))]);
    assert_eq!(layout.focus(), Some(w(1)));
    let layout = run(layout, [Event::OverviewClose, ALT_TAB, Event::Hover(w(2))]);
    assert_eq!(layout.focus(), Some(w(1)));
}

#[test]
fn req_layout_004_dialog_stays_with_its_parent() {
    let layout = run(start(), [open(1), map(2, None, Some(1), false)]);
    assert_eq!(tiles(&layout), [vec![1], vec![]]);
    assert_eq!(layout.home(w(2)), Some(0));
    // The dialog of the focused window takes the keyboard from its parent.
    assert_eq!(layout.focus(), Some(w(2)));
    let work = rect(0, STRIP, WORK.w, WORK.h);
    let dialog = Placement {
        window: w(2),
        rect: work,
        dialog: true,
    };
    assert_eq!(layout.project().windows, [tile(1, work), dialog]);
    assert_eq!(layout.configure(w(2)).unwrap().size, None);

    let closed = run(layout, [Event::Unmap(w(2))]);
    assert_eq!(closed.focus(), Some(w(1)));
}

#[test]
fn req_layout_004_closing_a_dialog_returns_the_keyboard_to_its_parent() {
    let pair = [open(1), open(2), Event::SplitWith(w(1)), Event::FocusRight];
    let layout = run(
        start(),
        pair.into_iter().chain([map(3, None, Some(1), false)]),
    );
    // [2 1]* with a dialog above 1; of the two columns, 2 was used last.
    assert_eq!((mru(&layout), layout.focus()), (vec![3, 2, 1], Some(w(3))));
    let closed = run(layout, [Event::Unmap(w(3))]);
    assert_eq!(closed.focus(), Some(w(1)));
}

#[test]
fn req_layout_004_focused_dialog_is_drawn_above_every_sibling_branch() {
    let nested = [map(2, None, Some(1), false), map(3, None, Some(2), false)];
    let layout = run(start(), [open(1)].into_iter().chain(nested));
    // 1 has the dialog 2, which has the dialog 3. A second dialog of 1 is
    // activated: nothing of the older branch may cover it.
    let layout = run(layout, [map(4, None, Some(1), true)]);
    assert_eq!(layout.focus(), Some(w(4)));
    assert_eq!(drawn(&layout), [1, 2, 3, 4]);

    // Raising 2 raises its branch; its own dialog stays above it.
    let layout = run(layout, [Event::FocusWindow(w(2))]);
    assert_eq!(drawn(&layout), [1, 4, 2, 3]);
    // Once the focus is gone, the branches keep the order of their use.
    let used = [Event::FocusSettled(w(2)), Event::FocusWindow(w(1))];
    assert_eq!(drawn(&run(layout, used)), [1, 4, 2, 3]);
}

#[test]
fn req_layout_004_dialog_of_a_background_window_waits_there() {
    let layout = run(start(), [open(1), open(2), map(3, None, Some(1), false)]);
    // [1 +3] [2]* []
    assert_eq!(tiles(&layout), [vec![1], vec![2], vec![]]);
    assert_eq!((layout.active(), layout.focus()), (1, Some(w(2))));
    assert_eq!(layout.home(w(3)), Some(0));
    let layout = run(layout, [Event::WorkspaceUp]);
    assert_eq!(layout.focus(), Some(w(3)));
}

#[test]
fn req_layout_004_orphaned_dialog_inherits_the_slot() {
    let dialogs = [map(2, None, Some(1), false), map(3, None, Some(1), false)];
    let layout = run(start(), [open(1)].into_iter().chain(dialogs));
    let layout = run(layout, [map(4, None, Some(3), false), Event::Unmap(w(1))]);
    assert_eq!(tiles(&layout), [vec![2], vec![]]);
    assert_eq!(layout.parent(w(2)), None);
    assert_eq!(layout.parent(w(3)), Some(w(2)));
    assert_eq!(layout.focus(), Some(w(2)));

    // A dialog in the middle of a chain hands its own dialogs to its parent.
    let layout = run(layout, [Event::Unmap(w(3))]);
    assert_eq!(layout.parent(w(4)), Some(w(2)));
}

#[test]
fn req_layout_005_columns_change_only_by_split_swap_and_unsplit() {
    let layout = run(start(), [open(1), open(2), open(3)]);
    let layout = run(layout, [Event::SplitWith(w(1))]);
    // [2] [3 1]* []
    assert_eq!(tiles(&layout), [vec![2], vec![3, 1], vec![]]);
    assert_eq!((layout.active(), layout.focus()), (1, Some(w(3))));
    assert_eq!(layout.configure(w(3)), Some(configure(HALF, true)));
    assert_eq!(layout.configure(w(1)), Some(configure(HALF, false)));

    // A full workspace takes no third column.
    assert_eq!(run(layout.clone(), [Event::SplitWith(w(2))]), layout);

    let layout = run(layout, [Event::SwapColumns]);
    assert_eq!(tiles(&layout), [vec![2], vec![1, 3], vec![]]);
    assert_eq!(layout.focus(), Some(w(3)));

    let layout = run(layout, [Event::FocusLeft]);
    assert_eq!(layout.focus(), Some(w(1)));
    let layout = run(layout, [Event::FocusLeft]);
    assert_eq!(layout.focus(), Some(w(1)));
    let layout = run(layout, [Event::FocusRight]);
    assert_eq!(layout.focus(), Some(w(3)));

    let layout = run(layout, [Event::Unsplit]);
    // [2] [1] [3]* []
    assert_eq!(tiles(&layout), [vec![2], vec![1], vec![3], vec![]]);
    assert_eq!((layout.active(), layout.focus()), (2, Some(w(3))));
    assert_eq!(layout.configure(w(1)), Some(configure(WORK, false)));
    assert_eq!(run(layout.clone(), [Event::Unsplit]), layout);
}

#[test]
fn req_layout_005_focus_left_and_right_return_to_the_dialog_of_a_column() {
    let pair = [open(1), open(2), Event::SplitWith(w(1))];
    let layout = run(start(), pair.into_iter().chain([Event::FocusRight]));
    // [2 1]*, with a dialog above 1
    let layout = run(layout, [map(3, None, Some(1), false), Event::FocusLeft]);
    assert_eq!(layout.focus(), Some(w(2)));
    let layout = run(layout, [Event::FocusRight]);
    assert_eq!(layout.focus(), Some(w(3)));
    // The sole column of the focus is not a move.
    let layout = run(layout, [Event::FocusRight]);
    assert_eq!(layout.focus(), Some(w(3)));
}

#[test]
fn req_ux_010_workspace_steps_saturate_and_numbers_clamp() {
    let layout = run(start(), [open(1), open(2), open(3)]);
    // [1] [2] [3]* []
    let layout = run(layout, [Event::WorkspaceDown, Event::WorkspaceDown]);
    assert_eq!((layout.active(), layout.focus()), (3, None));
    let layout = run(layout, vec![Event::WorkspaceUp; 5]);
    assert_eq!((layout.active(), layout.focus()), (0, Some(w(1))));
    let layout = run(layout, [Event::WorkspaceIndex(99)]);
    assert_eq!(layout.active(), 3);
    // Clamped first: on the last workspace such a number means "this one".
    let layout = run(layout, [Event::WorkspaceIndex(99)]);
    assert_eq!(layout.active(), 0);
}

#[test]
fn req_ux_010_previous_workspace_and_back_and_forth() {
    let layout = run(start(), [open(1), open(2), open(3)]);
    let layout = run(layout, [Event::WorkspaceIndex(0)]);
    assert_eq!(layout.active(), 0);
    // Selecting the current number goes back to where the user came from.
    let layout = run(layout, [Event::WorkspaceIndex(0)]);
    assert_eq!(layout.active(), 2);
    let layout = run(layout, [Event::WorkspacePrevious]);
    assert_eq!(layout.active(), 0);
    let layout = run(layout, [Event::WorkspacePrevious]);
    assert_eq!(layout.active(), 2);
}

#[test]
fn req_layout_013_one_empty_workspace_at_the_end_and_none_left_behind() {
    assert_eq!(tiles(&start()), [vec![]]);

    let emptied = [open(1), open(2), Event::WorkspaceUp, Event::Unmap(w(1))];
    let layout = run(start(), emptied);
    // []* [2] []: the user is still looking at the emptied workspace.
    assert_eq!(tiles(&layout), [vec![], vec![2], vec![]]);
    assert_eq!((layout.active(), layout.focus()), (0, None));
    let kept = layout.workspaces()[1].id;

    let (layout, effects) = step(layout, Event::WorkspaceDown);
    assert_eq!(tiles(&layout), [vec![2], vec![]]);
    assert_eq!((layout.active(), layout.focus()), (0, Some(w(2))));
    assert_eq!(layout.workspaces()[0].id, kept);
    assert!(effects.contains(&Effect::Workspaces));

    // The workspace the user came from is gone, so there is no way back.
    assert_eq!(run(layout.clone(), [Event::WorkspacePrevious]), layout);
}

#[test]
fn req_layout_015_moving_a_lone_window_reorders_workspaces() {
    let layout = run(start(), [open(1), open(2), open(3)]);
    let moved = layout.workspaces()[2].id;
    let layout = run(layout, [Event::MoveWindowUp]);
    // [1] [3]* [2] []
    assert_eq!(tiles(&layout), [vec![1], vec![3], vec![2], vec![]]);
    assert_eq!((layout.active(), layout.focus()), (1, Some(w(3))));
    assert_eq!(layout.workspaces()[1].id, moved);

    let top = run(layout, [Event::MoveWindowUp, Event::MoveWindowUp]);
    assert_eq!(tiles(&top), [vec![3], vec![1], vec![2], vec![]]);
    // The empty workspace at the end never trades places.
    let bottom = run(top, vec![Event::MoveWindowDown; 2]);
    assert_eq!(tiles(&bottom), [vec![1], vec![2], vec![3], vec![]]);
    assert_eq!(bottom.active(), 2);
    assert_eq!(run(bottom.clone(), [Event::MoveWindowDown]), bottom);
}

#[test]
fn req_layout_015_moving_a_column_of_a_pair_never_joins_a_neighbor() {
    let pair = run(start(), [open(1), open(2), open(3), Event::SplitWith(w(2))]);
    // [1] [3 2]* []
    let up = run(pair.clone(), [Event::MoveWindowUp]);
    assert_eq!(tiles(&up), [vec![1], vec![3], vec![2], vec![]]);
    assert_eq!((up.active(), up.focus()), (1, Some(w(3))));
    // The workspace it left is the previous one.
    let back = run(up, [Event::WorkspacePrevious]);
    assert_eq!((back.active(), back.focus()), (2, Some(w(2))));
    let down = run(pair, [Event::MoveWindowDown]);
    assert_eq!(tiles(&down), [vec![1], vec![2], vec![3], vec![]]);
    assert_eq!((down.active(), down.focus()), (2, Some(w(3))));
}

#[test]
fn req_layout_014_fullscreen_covers_the_output_and_restores_the_pair() {
    let pair = run(start(), [open(1), open(2), Event::SplitWith(w(1))]);
    // [2 1]*
    let (full, effects) = step(pair.clone(), Event::ToggleFullscreen);
    let state = Configure {
        size: Some(OUTPUT),
        activated: true,
        fullscreen: true,
    };
    assert_eq!(effects, [Effect::Configure(w(2), state)]);
    // The sibling keeps its size and is not drawn.
    assert_eq!(full.project().windows, [tile(2, rect(0, 0, 1000, 800))]);

    assert_eq!(run(full.clone(), [Event::ToggleFullscreen]), pair);
    let sibling = run(full.clone(), [Event::FocusRight]);
    assert_eq!(sibling.focus(), Some(w(1)));
    assert_eq!(sibling.configure(w(2)), Some(configure(HALF, false)));

    // Workspace navigation leaves and returns without ending fullscreen.
    let away = run(full.clone(), [Event::WorkspaceDown, Event::WorkspaceUp]);
    assert_eq!(away.workspaces(), full.workspaces());

    // The pointer cannot enter a window that is not drawn.
    assert_eq!(
        step(full.clone(), Event::Hover(w(1))),
        (full.clone(), vec![])
    );

    let alone = run(full, [Event::Unmap(w(1)), Event::ToggleFullscreen]);
    assert_eq!(alone.configure(w(2)), Some(configure(WORK, true)));
}

#[test]
fn req_layout_008_windows_get_the_output_below_the_reserved_strip() {
    let layout = run(start(), [open(1)]);
    let work = rect(0, STRIP, WORK.w, WORK.h);
    assert_eq!(layout.work_area(), work);
    assert_eq!(layout.project().windows, [tile(1, work)]);

    let resized = Event::Output {
        size: Size { w: 1200, h: 900 },
        reserved_top: 0,
    };
    let (layout, effects) = step(layout, resized);
    let size = Size { w: 1200, h: 900 };
    assert_eq!(effects, [Effect::Configure(w(1), configure(size, true))]);

    // Sizes that no output has still leave a work area.
    let nonsense = Event::Output {
        size: Size { w: 0, h: -5 },
        reserved_top: 99,
    };
    assert_eq!(run(layout, [nonsense]).work_area(), rect(0, 0, 1, 1));
}

#[test]
fn req_layout_016_window_beyond_the_limit_is_closed_and_long_app_id_dropped() {
    let background = (0..).take(MAX_WINDOWS).map(|id| map(id, None, None, false));
    let full = run(start(), background);
    assert_eq!(full.mru().len(), MAX_WINDOWS);
    assert_eq!(
        step(full.clone(), open(9999)),
        (full, vec![Effect::Close(w(9999))])
    );

    // Without an application id Alt+` has nothing to filter by.
    let long = "a".repeat(MAX_APP_ID_LEN + 1);
    let layout = run(start(), [app(1, "other"), app(2, &long), app(3, &long)]);
    assert_eq!(candidates(&run(layout, [ALT_GRAVE])), [3, 2, 1]);
}

#[test]
fn req_layout_017_effects_report_what_a_step_changed() {
    let (one, effects) = step(start(), open(1));
    let focused = Effect::Configure(w(1), configure(WORK, true));
    assert_eq!(
        effects,
        [focused, Effect::Focus(Some(w(1))), Effect::Workspaces]
    );

    let (two, effects) = step(one, open(2));
    let expected = [
        Effect::Configure(w(1), configure(WORK, false)),
        Effect::Configure(w(2), configure(WORK, true)),
        Effect::Focus(Some(w(2))),
        Effect::Workspaces,
    ];
    assert_eq!(effects, expected);

    // Closing is a request: the window stays until the client unmaps it.
    assert_eq!(
        step(two.clone(), Event::Close),
        (two, vec![Effect::Close(w(2))])
    );
    assert_eq!(step(start(), Event::Close), (start(), vec![]));
}

#[test]
fn req_ux_002_alt_tab_walks_the_focus_history_and_wraps() {
    let layout = run(start(), [open(1), open(2), open(3)]);
    assert_eq!(mru(&layout), [3, 2, 1]);
    let layout = run(layout, [ALT_TAB]);
    assert_eq!(
        (candidates(&layout), selected(&layout)),
        (vec![3, 2, 1], Some(2))
    );
    let layout = run(layout, [ALT_TAB]);
    assert_eq!(selected(&layout), Some(1));
    let layout = run(layout, [ALT_TAB]);
    assert_eq!(selected(&layout), Some(3));
    let layout = run(layout, [ALT_SHIFT_TAB]);
    assert_eq!(selected(&layout), Some(1));
    // Nothing moves until the modifier is released.
    assert_eq!((layout.active(), layout.focus()), (2, Some(w(3))));

    let opened_backwards = run(start(), [open(1), open(2), open(3), ALT_SHIFT_TAB]);
    assert_eq!(selected(&opened_backwards), Some(1));
}

#[test]
fn req_ux_002_commit_moves_workspace_and_focus_in_one_step() {
    let layout = run(start(), [open(1), open(2)]);
    let (layout, effects) = step(run(layout, [ALT_TAB]), Event::SwitcherCommit);
    assert_eq!((layout.active(), layout.focus()), (0, Some(w(1))));
    assert_eq!(layout.switcher(), None);
    assert!(effects.contains(&Effect::Focus(Some(w(1)))));
    assert!(effects.contains(&Effect::Workspaces));

    // Two windows alternate, on whatever workspaces they are.
    let layout = run(layout, [ALT_TAB, Event::SwitcherCommit]);
    assert_eq!((layout.active(), layout.focus()), (1, Some(w(2))));
    let layout = run(layout, [ALT_TAB, Event::SwitcherCommit]);
    assert_eq!((layout.active(), layout.focus()), (0, Some(w(1))));

    assert_eq!(
        run(layout.clone(), [ALT_TAB, Event::SwitcherCancel]),
        layout
    );
}

#[test]
fn req_ux_002_history_records_settled_focus_only() {
    let layout = run(start(), [open(1), open(2), open(3)]);
    // Passing over windows does not reorder the history.
    let passing = [Event::FocusWindow(w(1)), Event::FocusWindow(w(2))];
    let layout = run(layout, passing);
    assert_eq!(mru(&layout), [3, 2, 1]);
    // A timer armed for a window that lost the focus since is ignored.
    let layout = run(layout, [Event::FocusSettled(w(1))]);
    assert_eq!(mru(&layout), [3, 2, 1]);
    let layout = run(layout, [Event::FocusSettled(w(2))]);
    assert_eq!(mru(&layout), [2, 3, 1]);

    // Opening the switcher records the pending focus first.
    let switching = run(layout.clone(), [Event::FocusWindow(w(1)), ALT_TAB]);
    assert_eq!(candidates(&switching), [1, 2, 3]);
    assert_eq!(selected(&switching), Some(2));

    // A window that never had the focus is last, and its first focus counts
    // at once.
    let layout = run(layout, [map(4, None, None, false)]);
    assert_eq!(mru(&layout), [2, 3, 1, 4]);
    let layout = run(layout, [Event::FocusWindow(w(4))]);
    assert_eq!(mru(&layout), [4, 2, 3, 1]);
}

#[test]
fn req_ux_002_without_a_focused_window_the_most_recent_one_is_first() {
    assert_eq!(run(start(), [ALT_TAB]).switcher(), None);

    let layout = run(start(), [open(1), open(2), Event::WorkspaceDown]);
    assert_eq!(layout.focus(), None);
    let layout = run(layout, [ALT_TAB]);
    assert_eq!(selected(&layout), Some(2));
    let layout = run(layout, [Event::SwitcherCommit]);
    assert_eq!((layout.active(), layout.focus()), (1, Some(w(2))));
}

#[test]
fn req_ux_002_closed_candidate_leaves_the_list_and_the_last_one_cancels() {
    let layout = run(start(), [open(1), open(2), open(3), ALT_TAB]);
    let layout = run(layout, [Event::Unmap(w(2))]);
    assert_eq!(
        (candidates(&layout), selected(&layout)),
        (vec![3, 1], Some(1))
    );
    let layout = run(layout, [Event::Unmap(w(1))]);
    assert_eq!((candidates(&layout), selected(&layout)), (vec![3], Some(3)));
    let layout = run(layout, [Event::Unmap(w(3))]);
    assert_eq!(layout.switcher(), None);
}

#[test]
fn req_ux_003_alt_grave_cycles_windows_of_the_same_app() {
    let apps = [app(1, "term"), app(2, "web"), app(3, "term"), app(4, "web")];
    let layout = run(start(), apps);
    let layout = run(layout, [ALT_GRAVE]);
    assert_eq!(
        (candidates(&layout), selected(&layout)),
        (vec![4, 2], Some(2))
    );
    let layout = run(layout, [ALT_GRAVE]);
    assert_eq!(selected(&layout), Some(4));

    // Alt+Tab in the same session drops the filter and moves on.
    let all = run(layout.clone(), [ALT_TAB]);
    assert_eq!(
        (candidates(&all), selected(&all)),
        (vec![4, 3, 2, 1], Some(3))
    );
    // Alt+` after it filters by the window selected by then.
    let term = run(all, [ALT_GRAVE]);
    assert_eq!((candidates(&term), selected(&term)), (vec![3, 1], Some(1)));

    // The same application on another workspace is reached like any other.
    let layout = run(layout, [ALT_GRAVE, Event::SwitcherCommit]);
    assert_eq!((layout.active(), layout.focus()), (1, Some(w(2))));
}

#[test]
fn req_ux_003_single_match_stays_and_a_missing_app_id_is_no_filter() {
    let layout = run(start(), [app(1, "term"), app(2, "web"), app(3, "solo")]);
    let layout = run(layout, [ALT_GRAVE, ALT_GRAVE, ALT_GRAVE]);
    assert_eq!((candidates(&layout), selected(&layout)), (vec![3], Some(3)));

    let unnamed = run(start(), [app(1, "term"), open(2), ALT_GRAVE]);
    assert_eq!(
        (candidates(&unnamed), selected(&unnamed)),
        (vec![2, 1], Some(1))
    );
}

#[test]
fn req_ux_004_overview_shows_half_size_cards_around_the_active_one() {
    let normal = run(start(), [open(1), open(2), open(3), Event::SplitWith(w(1))]);
    // [2] [3 1]* []
    let (layout, effects) = step(normal, Event::OverviewOpen);
    // The overview resizes no client.
    assert_eq!(effects, vec![]);
    let ids: Vec<_> = layout.workspaces().iter().map(|ws| ws.id).collect();
    let cards = [
        rect(250, -240, 500, 400),
        rect(250, 200, 500, 400),
        rect(250, 640, 500, 400),
    ];
    let expected = Projection {
        workspaces: ids.into_iter().zip(cards).collect(),
        windows: vec![
            tile(2, rect(250, -220, 500, 380)),
            tile(3, rect(250, 220, 250, 380)),
            tile(1, rect(500, 220, 250, 380)),
        ],
    };
    assert_eq!(layout.project(), expected);
}

#[test]
fn req_ux_004_overview_is_navigated_with_the_ordinary_commands() {
    let layout = run(start(), [open(1), open(2), open(3), Event::SplitWith(w(1))]);
    // [2] [3 1]* []
    let layout = run(layout, [Event::OverviewToggle, Event::FocusRight]);
    assert_eq!((layout.overview(), layout.focus()), (true, Some(w(1))));
    let layout = run(layout, [Event::WorkspaceUp]);
    assert_eq!((layout.overview(), layout.focus()), (true, Some(w(2))));
    // Choosing a window leaves the overview on it.
    let layout = run(layout, [Event::FocusWindow(w(3))]);
    assert_eq!((layout.overview(), layout.focus()), (false, Some(w(3))));
    let layout = run(layout, [Event::OverviewToggle, Event::OverviewToggle]);
    assert!(!layout.overview());
}

#[test]
fn req_layout_009_closing_the_overview_untouched_restores_the_state() {
    let layout = run(start(), [open(1), open(2), Event::SplitWith(w(1))]);
    let round_trip = [Event::OverviewOpen, Event::OverviewClose];
    assert_eq!(run(layout.clone(), round_trip), layout);
}

#[test]
fn req_layout_018_projection_places_every_workspace_in_one_space() {
    let layout = run(start(), [open(1), open(2), open(3), Event::SplitWith(w(1))]);
    // [2] [3 1]* []
    let ids: Vec<_> = layout.workspaces().iter().map(|ws| ws.id).collect();
    let areas = [
        rect(0, -800, 1000, 800),
        rect(0, 0, 1000, 800),
        rect(0, 800, 1000, 800),
    ];
    let windows = vec![
        tile(2, rect(0, -760, 1000, 760)),
        tile(3, rect(0, 40, 500, 760)),
        tile(1, rect(500, 40, 500, 760)),
    ];
    let expected = Projection {
        workspaces: ids.into_iter().zip(areas).collect(),
        windows,
    };
    assert_eq!(layout.project(), expected);

    // An odd width is shared without a gap or an overlap.
    let odd = Event::Output {
        size: Size { w: 1001, h: 800 },
        reserved_top: STRIP,
    };
    let windows = run(layout, [odd]).project().windows;
    assert_eq!(windows[1].rect, rect(0, 40, 500, 760));
    assert_eq!(windows[2].rect, rect(500, 40, 501, 760));
}

#[test]
fn req_layout_013_last_window_closing_keeps_one_trailing_workspace() {
    let layout = run(start(), [open(1), open(2)]);
    let emptied = layout.workspaces()[layout.active()].id;
    let layout = run(layout, [Event::Unmap(w(2))]);
    assert_eq!(tiles(&layout), [vec![1], vec![]]);
    assert_eq!(layout.workspaces()[layout.active()].id, emptied);
    assert_eq!(run(layout.clone(), [Event::WorkspaceDown]), layout);
}
