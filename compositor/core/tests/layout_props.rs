//! Layout invariants over generated operation sequences.
//!
//! The run starts from a fixed seed so that the merge gate gives the same
//! answer every time; `PROPTEST_RNG_SEED` and `PROPTEST_CASES` override the
//! seed and the case count for a local hunt. A failing case is shrunk and
//! written to `layout_props.proptest-regressions` next to this file, and is
//! replayed first on every later run: commit that file with the fix.

#![expect(
    clippy::unwrap_used,
    reason = "clippy.toml exempts test functions only; the helpers here are test code too"
)]

use std::collections::{BTreeMap, BTreeSet};

use proptest::collection::vec;
use proptest::prelude::*;
use proptest::sample::select;
use proptest::test_runner::{FileFailurePersistence, RngSeed};
use skorupa_compositor_core::layout::{
    Configure, Effect, Event, Layout, MAX_OUTPUT_SIDE, Rect, Size, WindowId, WorkspaceId, step,
};

fn config() -> ProptestConfig {
    ProptestConfig {
        cases: 1024,
        rng_seed: RngSeed::Fixed(0x534b_4f52_5550_4c41),
        // The default looks for `src/lib.rs` above the test and finds none.
        failure_persistence: Some(Box::new(FileFailurePersistence::WithSource(
            "proptest-regressions",
        ))),
        ..ProptestConfig::default()
    }
}

/// Few ids, so that events hit mapped windows, parents and duplicates often.
fn window() -> impl Strategy<Value = WindowId> {
    (0u64..10).prop_map(WindowId)
}

fn event() -> impl Strategy<Value = Event> {
    let app_id = select(vec![None, Some("a".to_owned()), Some("b".to_owned())]);
    let parent = (proptest::option::of(window()), any::<bool>());
    let map = (window(), app_id, parent, any::<bool>());
    let side = prop_oneof![-2..=3000i32, MAX_OUTPUT_SIDE - 1..=MAX_OUTPUT_SIDE + 1];
    let commands = select(vec![
        Event::FocusLeft,
        Event::FocusRight,
        Event::WorkspaceUp,
        Event::WorkspaceDown,
        Event::WorkspacePrevious,
        Event::MoveWindowUp,
        Event::MoveWindowDown,
        Event::Unsplit,
        Event::SwapColumns,
        Event::ToggleFullscreen,
        Event::Close,
        Event::OverviewOpen,
        Event::OverviewClose,
        Event::OverviewToggle,
        Event::SwitcherCommit,
        Event::SwitcherCancel,
    ]);
    prop_oneof![
        5 => map.prop_map(|(window, app_id, (parent, parent_same_client), valid_token)| {
            Event::Map { window, app_id, parent, parent_same_client, valid_token }
        }),
        2 => window().prop_map(Event::Unmap),
        1 => (window(), any::<bool>())
            .prop_map(|(window, valid_token)| Event::Activate { window, valid_token }),
        1 => window().prop_map(Event::FocusWindow),
        1 => window().prop_map(Event::Hover),
        1 => window().prop_map(Event::FocusSettled),
        2 => window().prop_map(Event::SplitWith),
        1 => (0usize..8).prop_map(Event::WorkspaceIndex),
        2 => (any::<bool>(), any::<bool>())
            .prop_map(|(same_app, reverse)| Event::SwitcherStep { same_app, reverse }),
        1 => (side.clone(), side, -2..200i32).prop_map(|(w, h, reserved_top)| Event::Output {
            size: Size { w, h },
            reserved_top,
        }),
        8 => commands,
    ]
}

/// The tile at the root of the window's parent chain. The hop limit makes a
/// cycle a failure instead of a hang.
fn root(layout: &Layout, window: WindowId) -> WindowId {
    let mut hops = layout.mru().len();
    let mut root = window;
    while let Some(parent) = layout.parent(root) {
        hops = hops.checked_sub(1).unwrap();
        root = parent;
    }
    root
}

/// A rectangle of negative size is inside nothing.
fn inside(inner: Rect, outer: Rect) -> bool {
    inner.w >= 0
        && inner.h >= 0
        && inner.x >= outer.x
        && inner.y >= outer.y
        && inner.x + inner.w <= outer.x + outer.w
        && inner.y + inner.h <= outer.y + outer.h
}

/// Whether `window` is `ancestor` or one of its dialogs, at any depth.
fn descends(layout: &Layout, window: WindowId, ancestor: WindowId) -> bool {
    let mut hops = layout.mru().len();
    let mut at = Some(window);
    while let Some(current) = at.filter(|&current| current != ancestor) {
        hops = hops.checked_sub(1).unwrap();
        at = layout.parent(current);
    }
    at.is_some()
}

/// The focus that a map or an activation request without a valid token
/// leaves: the one before, with the two exceptions of REQ-LAYOUT-006.
fn focus_after_tokenless(before: &Layout, event: &Event) -> Option<WindowId> {
    let mapped = |window: WindowId| before.home(window).is_some();
    match *event {
        Event::Map {
            window,
            parent,
            parent_same_client,
            ..
        } if !mapped(window) => {
            let dialog = parent.filter(|&parent| mapped(parent));
            // A dialog takes the keyboard from its own parent, if both
            // belong to one client.
            let from_parent = dialog.is_some() && dialog == before.focus() && parent_same_client;
            // Nothing is focused: an independent window may as well be.
            let unopposed = dialog.is_none() && before.focus().is_none();
            if from_parent || unopposed {
                Some(window)
            } else {
                before.focus()
            }
        }
        _ => before.focus(),
    }
}

/// Sharing an edge is not overlapping.
fn overlap(a: Rect, b: Rect) -> bool {
    a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h
}

/// What the shell shows of the workspaces.
fn shell(layout: &Layout) -> (Vec<(WorkspaceId, bool)>, usize) {
    let occupied = |ws: &skorupa_compositor_core::layout::Workspace| (ws.id, !ws.tiles.is_empty());
    (
        layout.workspaces().iter().map(occupied).collect(),
        layout.active(),
    )
}

fn pairs(layout: &Layout) -> usize {
    let pair = |ws: &&skorupa_compositor_core::layout::Workspace| ws.tiles.len() == 2;
    layout.workspaces().iter().filter(pair).count()
}

/// State invariants. `mapped` is the test's own record of what is mapped.
fn check_state(layout: &Layout, mapped: &BTreeSet<WindowId>) -> Result<(), TestCaseError> {
    let spaces = layout.workspaces();
    let tiles: Vec<WindowId> = spaces.iter().flat_map(|ws| ws.tiles.clone()).collect();

    // The focus history holds exactly the mapped windows, each once.
    prop_assert_eq!(layout.mru().len(), mapped.len());
    prop_assert_eq!(
        &layout.mru().iter().copied().collect::<BTreeSet<_>>(),
        mapped
    );

    // REQ-LAYOUT-007: one list, distinct ids, a valid active position.
    prop_assert!(layout.active() < spaces.len());
    let ids: BTreeSet<WorkspaceId> = spaces.iter().map(|ws| ws.id).collect();
    prop_assert_eq!(ids.len(), spaces.len());

    for (index, ws) in spaces.iter().enumerate() {
        // REQ-LAYOUT-002
        prop_assert!(ws.tiles.len() <= 2);
        // REQ-LAYOUT-013: empty at the end, and otherwise only under the user.
        if index + 1 == spaces.len() {
            prop_assert!(ws.tiles.is_empty());
            prop_assert!(index == 0 || !spaces[index - 1].tiles.is_empty());
        } else {
            prop_assert!(!ws.tiles.is_empty() || index == layout.active());
        }
        // A workspace with windows remembers one of its own as focused.
        prop_assert_eq!(ws.focus.is_some(), !ws.tiles.is_empty());
        if let Some(focus) = ws.focus {
            prop_assert_eq!(layout.home(focus), Some(index));
        }
        // REQ-LAYOUT-014: only the focused tile is fullscreen.
        if let Some(fullscreen) = ws.fullscreen {
            prop_assert!(ws.tiles.contains(&fullscreen));
            prop_assert_eq!(ws.focus.map(|focus| root(layout, focus)), Some(fullscreen));
        }
    }

    for &window in mapped {
        // Every window is in exactly one workspace, through exactly one tile.
        let tile = root(layout, window);
        prop_assert_eq!(tiles.iter().filter(|&&t| t == tile).count(), 1);
        let home = spaces.iter().position(|ws| ws.tiles.contains(&tile));
        prop_assert_eq!(layout.home(window), home);
        match layout.parent(window) {
            // REQ-LAYOUT-004: a dialog is where its parent is.
            Some(parent) => {
                prop_assert!(mapped.contains(&parent));
                prop_assert_eq!(layout.home(parent), home);
            }
            // REQ-LAYOUT-012: an independent window always has a slot.
            None => prop_assert!(tiles.contains(&window)),
        }
    }
    prop_assert!(tiles.iter().all(|tile| mapped.contains(tile)));

    // Exactly one focused window while the active workspace shows any.
    prop_assert_eq!(layout.focus(), spaces[layout.active()].focus);
    let activated = |window: &&WindowId| layout.configure(**window).unwrap().activated;
    prop_assert_eq!(
        mapped.iter().filter(activated).count(),
        usize::from(layout.focus().is_some())
    );

    if let Some((candidates, selected)) = layout.switcher() {
        prop_assert!(candidates.contains(&selected));
        prop_assert!(candidates.iter().all(|window| mapped.contains(window)));
    }
    Ok(())
}

/// REQ-LAYOUT-018: geometry invariants of the projection.
fn check_projection(layout: &Layout, mapped: &BTreeSet<WindowId>) -> Result<(), TestCaseError> {
    let spaces = layout.workspaces();
    let projection = layout.project();
    let Size { w, h } = layout.output();
    let output = Rect { x: 0, y: 0, w, h };

    let ids: Vec<WorkspaceId> = projection.workspaces.iter().map(|(id, _)| *id).collect();
    prop_assert_eq!(ids, spaces.iter().map(|ws| ws.id).collect::<Vec<_>>());
    let areas: Vec<Rect> = projection
        .workspaces
        .iter()
        .map(|(_, area)| *area)
        .collect();
    for (index, area) in areas.iter().enumerate() {
        prop_assert!(areas[..index].iter().all(|above| !overlap(*above, *area)));
    }
    // The active workspace is the screen, or a card inside it.
    let active = areas[layout.active()];
    prop_assert!(inside(active, output));
    prop_assert!(layout.overview() || active == output);

    // Drawn are all windows, except those under a fullscreen tile.
    let drawn: Vec<WindowId> = projection.windows.iter().map(|p| p.window).collect();
    let visible = |window: &&WindowId| {
        let fullscreen = spaces[layout.home(**window).unwrap()].fullscreen;
        fullscreen.is_none_or(|tile| tile == root(layout, **window))
    };
    let expected: BTreeSet<WindowId> = mapped.iter().filter(visible).copied().collect();
    prop_assert_eq!(drawn.len(), expected.len());
    prop_assert_eq!(drawn.iter().copied().collect::<BTreeSet<_>>(), expected);

    for (index, placed) in projection.windows.iter().enumerate() {
        let below = &projection.windows[..index];
        // No window leaves its workspace, so none on screen leaves the output.
        prop_assert!(inside(
            placed.rect,
            areas[layout.home(placed.window).unwrap()]
        ));
        prop_assert_eq!(placed.dialog, layout.parent(placed.window).is_some());
        if let Some(parent) = layout.parent(placed.window) {
            // REQ-LAYOUT-004: above its parent, in the slot of its tile.
            let tile = root(layout, placed.window);
            prop_assert!(below.iter().any(|p| p.window == parent));
            prop_assert!(
                below
                    .iter()
                    .any(|p| p.window == tile && p.rect == placed.rect)
            );
        } else {
            let tiles = below.iter().filter(|p| !p.dialog);
            prop_assert!(tiles.clone().all(|p| !overlap(p.rect, placed.rect)));
        }
    }

    // REQ-LAYOUT-004: nothing of a sibling branch covers the window that has,
    // or on its workspace would have, the keyboard: above it in its tile are
    // only its own dialogs.
    for focus in spaces.iter().filter_map(|ws| ws.focus) {
        let at = drawn.iter().position(|&window| window == focus).unwrap();
        let tile = root(layout, focus);
        let same_tile = |window: &&WindowId| root(layout, **window) == tile;
        let mut above = drawn[at + 1..].iter().filter(same_tile);
        prop_assert!(above.all(|&window| descends(layout, window, focus)));
    }
    Ok(())
}

/// What the adapter knows from the effects alone.
#[derive(Default)]
struct Told {
    configures: BTreeMap<WindowId, Configure>,
    focus: Option<WindowId>,
}

/// REQ-LAYOUT-017: the effects of a step are exactly what it changed.
fn check_effects(
    told: &mut Told,
    before: &Layout,
    after: &Layout,
    effects: &[Effect],
) -> Result<(), TestCaseError> {
    let mut workspaces = false;
    for effect in effects {
        match effect {
            Effect::Configure(window, state) => {
                prop_assert_ne!(told.configures.insert(*window, *state), Some(*state));
            }
            Effect::Focus(focus) => {
                prop_assert_ne!(told.focus, *focus);
                told.focus = *focus;
            }
            Effect::Workspaces => workspaces = true,
            Effect::Close(_) => {}
        }
    }
    told.configures
        .retain(|window, _| after.home(*window).is_some());
    for (window, state) in &told.configures {
        prop_assert_eq!(after.configure(*window), Some(*state));
    }
    prop_assert_eq!(told.configures.len(), after.mru().len());
    prop_assert_eq!(told.focus, after.focus());
    prop_assert_eq!(workspaces, shell(before) != shell(after));
    Ok(())
}

proptest! {
    #![proptest_config(config())]

    /// REQ-LAYOUT-001: a step depends on nothing but the state and the event.
    /// After every step of any sequence the state, the projection and the
    /// effects keep the invariants of REQ-LAYOUT-002, REQ-LAYOUT-004,
    /// REQ-LAYOUT-007, REQ-LAYOUT-012, REQ-LAYOUT-013, REQ-LAYOUT-014,
    /// REQ-LAYOUT-017 and REQ-LAYOUT-018, and the two rules about clients:
    /// REQ-LAYOUT-005 (only the split command adds a column) and
    /// REQ-LAYOUT-006 (without a valid token a map or an activation request
    /// leaves the focused tile and the active workspace alone).
    #[test]
    fn req_layout_001_any_sequence_is_deterministic_and_keeps_the_invariants(
        events in vec(event(), 1..80),
    ) {
        let mut layout = Layout::new(Size { w: 1000, h: 800 }, 40);
        let mut mapped = BTreeSet::new();
        let mut told = Told::default();
        for event in events {
            let (after, effects) = step(layout.clone(), event.clone());
            prop_assert_eq!(&step(layout.clone(), event.clone()), &(after.clone(), effects.clone()));

            match event {
                Event::Map { window, .. } => drop(mapped.insert(window)),
                Event::Unmap(window) => drop(mapped.remove(&window)),
                _ => {}
            }
            check_state(&after, &mapped)?;
            check_projection(&after, &mapped)?;
            check_effects(&mut told, &layout, &after, &effects)?;

            prop_assert!(pairs(&after) <= pairs(&layout) || matches!(event, Event::SplitWith(_)));
            let tokenless = matches!(
                event,
                Event::Map { valid_token: false, .. } | Event::Activate { valid_token: false, .. }
            );
            if tokenless {
                let view = |l: &Layout| l.workspaces()[l.active()].id;
                prop_assert_eq!(view(&layout), view(&after));
                prop_assert_eq!(after.focus(), focus_after_tokenless(&layout, &event));
            }
            layout = after;
        }
    }

    /// REQ-LAYOUT-009: the overview keeps no state of its own, so opening
    /// and closing it without an action returns the state it was opened on.
    #[test]
    fn req_layout_009_overview_round_trip_changes_nothing(events in vec(event(), 0..40)) {
        let closed = events
            .into_iter()
            .chain([Event::OverviewClose])
            .fold(Layout::new(Size { w: 1000, h: 800 }, 40), |layout, event| step(layout, event).0);
        let (open, _) = step(closed.clone(), Event::OverviewOpen);
        prop_assert!(open.overview());
        prop_assert_eq!(step(open, Event::OverviewClose).0, closed);
    }
}
