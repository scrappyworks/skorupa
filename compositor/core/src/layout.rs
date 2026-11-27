//! Workspaces, windows, focus and the overview.
//!
//! [`step`] is the whole behavior. The adapter turns Wayland requests and
//! key bindings into [`Event`]s, carries out the returned [`Effect`]s and
//! draws what [`Layout::project`] says.
//!
//! # Model
//!
//! Workspaces form one vertical list with one active entry. A workspace holds
//! at most two *tiles*, left to right. A window without a parent is always a
//! tile; a window with a parent (a dialog) has no slot of its own and lives
//! wherever the tile at the root of its parent chain lives. Each workspace
//! remembers which of its windows has the keyboard, so the focused window is
//! simply the remembered one of the active workspace: with windows on screen
//! exactly one has focus, and on an empty workspace none has.
//!
//! Only the user adds a column ([`Event::SplitWith`]). A client can map,
//! unmap and ask for activation; without a valid activation token none of
//! these moves the focus to another tile or changes the active workspace.
//! The one exception is a dialog whose parent has the focus: it takes the
//! keyboard from its own parent.
//!
//! # Empty workspaces
//!
//! The rule is niri's rule for dynamic workspaces, minus named workspaces:
//! the list always ends with exactly one empty workspace, and any other empty
//! workspace exists only while it is the active one. It is removed in the
//! step in which the user leaves it, because nothing animates yet and so
//! navigation settles at once. Workspace ids are stable; positions are not.
//!
//! # Focus history
//!
//! [`Layout::mru`] lists every mapped window, most recently used first. A
//! window reaches the front on its first focus, on [`Event::FocusSettled`]
//! (the adapter's 750 ms timer, or a key press that reached the window), when
//! the switcher opens while it has the focus and when the switcher commits
//! it. Merely passing over windows therefore does not reorder the list.
//! Windows that never had the focus stay at the back.
//!
//! The switcher (Alt+Tab, Alt+grave) is a selection in that list, optionally
//! narrowed to one application id. It changes nothing until it commits, and
//! then the workspace and the focus change in the same step.
//!
//! The overview is one flag. It is navigated with the ordinary focus and
//! workspace commands and changes no rule, only the projection.
//!
//! # Geometry and animation
//!
//! Rectangles are logical pixels with the origin at the top-left corner of
//! the output, independent of the output scale. A [`Projection`] gives every
//! workspace and every visible window a rectangle in that one space, also
//! when it is off screen: workspaces are stacked below one another and the
//! active one covers the output, or sits in the middle as a half-size card
//! while the overview is open. Nothing here animates. An animation layer
//! keeps the projection from before a step and the one after it and
//! interpolates rectangle by rectangle, matching them by id: a workspace
//! switch becomes a vertical slide, the overview a zoom, a split a resize.
//! What exists in only one of the two projections is what that layer fades.

use std::collections::BTreeMap;

/// Windows the layout tracks at once. A window mapped beyond it is closed.
pub const MAX_WINDOWS: usize = 512;
/// Longest application id kept, in bytes. A longer one counts as absent.
pub const MAX_APP_ID_LEN: usize = 256;
/// Largest output width or height, in logical pixels.
pub const MAX_OUTPUT_SIDE: i32 = 16_384;

/// Chosen by the adapter, unique among mapped windows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WindowId(pub u64);

/// Assigned by the layout and never reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WorkspaceId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Size {
    pub w: i32,
    pub h: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// A client mapped a toplevel. An unknown `parent` counts as none.
    Map {
        window: WindowId,
        app_id: Option<String>,
        parent: Option<WindowId>,
        /// The map carries an activation token from a real user interaction.
        valid_token: bool,
    },
    Unmap(WindowId),
    /// A client asked for activation of a mapped window.
    Activate {
        window: WindowId,
        valid_token: bool,
    },
    /// The user picked a window: a click, a tap, a request from the shell.
    FocusWindow(WindowId),
    /// The pointer entered a window.
    Hover(WindowId),
    /// The window kept the focus long enough, or received a key press. The
    /// adapter starts that wait on every [`Effect::Focus`].
    FocusSettled(WindowId),
    FocusLeft,
    FocusRight,
    WorkspaceUp,
    WorkspaceDown,
    /// Zero-based position in the list.
    WorkspaceIndex(usize),
    WorkspacePrevious,
    MoveWindowUp,
    MoveWindowDown,
    /// Bring this window, with its dialogs, in as the right column of the
    /// active workspace. Nothing happens when that already has two.
    SplitWith(WindowId),
    /// Send the focused column of a pair to a new workspace below.
    Unsplit,
    SwapColumns,
    ToggleFullscreen,
    /// Ask the focused window to close.
    Close,
    OverviewOpen,
    OverviewClose,
    OverviewToggle,
    /// Alt+Tab (`same_app` false) or Alt+grave (true): open the switcher, or move
    /// its selection when it is open.
    SwitcherStep {
        same_app: bool,
        reverse: bool,
    },
    /// The last held modifier was released: focus the selection.
    SwitcherCommit,
    SwitcherCancel,
    /// The output size or the strip reserved at its top changed.
    Output {
        size: Size,
        reserved_top: i32,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Send this state to the window; it differs from the last one sent.
    Configure(WindowId, Configure),
    /// Keyboard focus moved.
    Focus(Option<WindowId>),
    /// The list the shell shows changed: read [`Layout::workspaces`] and
    /// [`Layout::active`].
    Workspaces,
    /// Ask the client to close this window.
    Close(WindowId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Configure {
    /// `None` for a dialog, which chooses its own size.
    pub size: Option<Size>,
    pub activated: bool,
    pub fullscreen: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Workspace {
    pub id: WorkspaceId,
    /// Columns from left to right, at most two.
    pub tiles: Vec<WindowId>,
    /// The window that has the keyboard while this workspace is active.
    pub focus: Option<WindowId>,
    /// The tile that covers the whole output; it is always the focused one.
    pub fullscreen: Option<WindowId>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Projection {
    /// Every workspace from top to bottom: its screen area, or its card in
    /// the overview.
    pub workspaces: Vec<(WorkspaceId, Rect)>,
    /// Every visible window, back to front.
    pub windows: Vec<Placement>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    pub window: WindowId,
    /// The slot of a tile. For a dialog, the slot of its tile: the adapter
    /// centers the dialog's own size inside it.
    pub rect: Rect,
    pub dialog: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Window {
    app_id: Option<String>,
    parent: Option<WindowId>,
    /// The first focus of a window enters the history without waiting.
    focused_before: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Switcher {
    selected: WindowId,
    /// The candidates are the windows of this application only.
    app_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    workspaces: Vec<Workspace>,
    active: usize,
    /// Where "previous workspace" goes, if it still exists.
    previous: Option<WorkspaceId>,
    next_workspace: u64,
    windows: BTreeMap<WindowId, Window>,
    mru: Vec<WindowId>,
    overview: bool,
    switcher: Option<Switcher>,
    output: Size,
    reserved_top: i32,
}

/// What the adapter and the shell were told last: the state of every window,
/// the focus, and the workspace list with its active position.
type Told = (
    BTreeMap<WindowId, Configure>,
    Option<WindowId>,
    (Vec<(WorkspaceId, bool)>, usize),
);

/// The overview draws every workspace at one half.
const OVERVIEW_SHRINK: i32 = 2;

/// Counts here are bounded by [`MAX_WINDOWS`], far below the fallback.
fn int(count: usize) -> i32 {
    i32::try_from(count).unwrap_or(i32::MAX)
}

/// Applies one event. The effects say what changed for the windows, the
/// keyboard and the shell, in that order, after any close request.
#[must_use]
pub fn step(mut state: Layout, event: Event) -> (Layout, Vec<Effect>) {
    let before = state.told();
    let mut effects = Vec::new();
    state.apply(event, &mut effects);
    state.settle();
    let after = state.told();
    for (window, configure) in &after.0 {
        if before.0.get(window) != Some(configure) {
            effects.push(Effect::Configure(*window, *configure));
        }
    }
    if before.1 != after.1 {
        effects.push(Effect::Focus(after.1));
    }
    if before.2 != after.2 {
        effects.push(Effect::Workspaces);
    }
    (state, effects)
}

impl Layout {
    #[must_use]
    pub fn new(output: Size, reserved_top: i32) -> Self {
        let mut layout = Self {
            workspaces: Vec::new(),
            active: 0,
            previous: None,
            next_workspace: 0,
            windows: BTreeMap::new(),
            mru: Vec::new(),
            overview: false,
            switcher: None,
            output,
            reserved_top,
        };
        layout.set_output(output, reserved_top);
        layout.insert_workspace(0, Vec::new(), None);
        layout.active = 0;
        layout
    }

    #[must_use]
    pub fn workspaces(&self) -> &[Workspace] {
        &self.workspaces
    }

    /// Position of the active workspace in [`Layout::workspaces`].
    #[must_use]
    pub fn active(&self) -> usize {
        self.active
    }

    #[must_use]
    pub fn focus(&self) -> Option<WindowId> {
        self.workspaces[self.active].focus
    }

    /// Every mapped window, most recently used first.
    #[must_use]
    pub fn mru(&self) -> &[WindowId] {
        &self.mru
    }

    #[must_use]
    pub fn parent(&self, window: WindowId) -> Option<WindowId> {
        self.windows.get(&window)?.parent
    }

    /// Position of the workspace that holds the window.
    #[must_use]
    pub fn home(&self, window: WindowId) -> Option<usize> {
        let tile = self.tile_of(window);
        self.workspaces
            .iter()
            .position(|ws| ws.tiles.contains(&tile))
    }

    #[must_use]
    pub fn overview(&self) -> bool {
        self.overview
    }

    /// The open switcher: its candidates in order, and the selected one.
    #[must_use]
    pub fn switcher(&self) -> Option<(Vec<WindowId>, WindowId)> {
        let switcher = self.switcher.as_ref()?;
        Some((self.candidates(switcher), switcher.selected))
    }

    #[must_use]
    pub fn output(&self) -> Size {
        self.output
    }

    /// Where a lone tile goes: the output without the reserved strip. Also
    /// the size to offer a new independent window before it maps.
    #[must_use]
    pub fn work_area(&self) -> Rect {
        Rect {
            x: 0,
            y: self.reserved_top,
            w: self.output.w,
            h: self.output.h - self.reserved_top,
        }
    }

    /// The state the window should have been sent last.
    #[must_use]
    pub fn configure(&self, window: WindowId) -> Option<Configure> {
        let ws = &self.workspaces[self.home(window)?];
        let column = ws.tiles.iter().position(|&tile| tile == window);
        Some(Configure {
            size: column.map(|column| {
                let Rect { w, h, .. } = self.slot(ws, column);
                Size { w, h }
            }),
            activated: self.focus() == Some(window),
            fullscreen: ws.fullscreen == Some(window),
        })
    }

    #[must_use]
    pub fn project(&self) -> Projection {
        let Size { w, h } = self.output;
        let shrink = if self.overview { OVERVIEW_SHRINK } else { 1 };
        let (card_w, card_h) = (w / shrink, h / shrink);
        // niri's overview gap, a tenth of the output height at card scale.
        let pitch = card_h + if self.overview { h / 10 / shrink } else { 0 };
        let (left, top) = (
            (w - card_w) / 2,
            (h - card_h) / 2 - int(self.active) * pitch,
        );

        // Dialogs of one tile, parents before children and the most recently
        // used of equals last, so that it is drawn on top.
        let mut dialogs: Vec<(WindowId, usize, usize, WindowId)> = Vec::new();
        for (age, &window) in self.mru.iter().rev().enumerate() {
            let depth = self.chain(window).count() - 1;
            if depth > 0 {
                dialogs.push((self.tile_of(window), depth, age, window));
            }
        }
        dialogs.sort_unstable();

        let mut projection = Projection::default();
        for (index, ws) in self.workspaces.iter().enumerate() {
            let y = top + int(index) * pitch;
            let area = Rect {
                x: left,
                y,
                w: card_w,
                h: card_h,
            };
            projection.workspaces.push((ws.id, area));
            for (column, &tile) in ws.tiles.iter().enumerate() {
                if ws.fullscreen.is_some_and(|fullscreen| fullscreen != tile) {
                    continue;
                }
                // Both edges are scaled, not the size: neighbors keep
                // sharing an edge whatever the rounding.
                let slot = self.slot(ws, column);
                let (x0, y0) = (slot.x / shrink, slot.y / shrink);
                let rect = Rect {
                    x: left + x0,
                    y: y + y0,
                    w: (slot.x + slot.w) / shrink - x0,
                    h: (slot.y + slot.h) / shrink - y0,
                };
                let placed = |window, dialog| Placement {
                    window,
                    rect,
                    dialog,
                };
                projection.windows.push(placed(tile, false));
                let above = dialogs.iter().filter(|dialog| dialog.0 == tile);
                projection
                    .windows
                    .extend(above.map(|dialog| placed(dialog.3, true)));
            }
        }
        projection
    }

    /// The slot of a column in the coordinates of its own workspace.
    fn slot(&self, ws: &Workspace, column: usize) -> Rect {
        let Size { w, h } = self.output;
        if ws.fullscreen == Some(ws.tiles[column]) {
            return Rect { x: 0, y: 0, w, h };
        }
        let (x, width) = match (ws.tiles.len(), column) {
            (2, 0) => (0, w / 2),
            (2, _) => (w / 2, w - w / 2),
            _ => (0, w),
        };
        Rect {
            x,
            w: width,
            ..self.work_area()
        }
    }

    /// The window and its parents up to the tile. It ends: a parent was
    /// mapped before its child and is only ever replaced by its own parent.
    fn chain(&self, window: WindowId) -> impl Iterator<Item = WindowId> {
        std::iter::successors(Some(window), |window| self.parent(*window))
    }

    fn tile_of(&self, window: WindowId) -> WindowId {
        self.chain(window).last().unwrap_or(window)
    }

    /// The most recently used window of a tile: itself or one of its dialogs.
    fn recent_in(&self, tile: WindowId) -> Option<WindowId> {
        self.mru
            .iter()
            .copied()
            .find(|&window| self.tile_of(window) == tile)
    }

    fn candidates(&self, switcher: &Switcher) -> Vec<WindowId> {
        let app_id = |window: &WindowId| self.windows.get(window)?.app_id.as_ref();
        let wanted = |window: &WindowId| {
            switcher.app_id.is_none() || app_id(window) == switcher.app_id.as_ref()
        };
        self.mru.iter().copied().filter(wanted).collect()
    }

    fn told(&self) -> Told {
        let configure = |window: &WindowId| Some((*window, self.configure(*window)?));
        let occupied = |ws: &Workspace| (ws.id, !ws.tiles.is_empty());
        (
            self.windows.keys().filter_map(configure).collect(),
            self.focus(),
            (self.workspaces.iter().map(occupied).collect(), self.active),
        )
    }

    fn set_output(&mut self, size: Size, reserved_top: i32) {
        self.output = Size {
            w: size.w.clamp(1, MAX_OUTPUT_SIDE),
            h: size.h.clamp(1, MAX_OUTPUT_SIDE),
        };
        // The strip never takes the whole output from the windows.
        self.reserved_top = reserved_top.clamp(0, self.output.h - 1);
    }

    /// Inserts a workspace; the active position keeps naming the same one.
    fn insert_workspace(&mut self, at: usize, tiles: Vec<WindowId>, focus: Option<WindowId>) {
        let id = WorkspaceId(self.next_workspace);
        self.next_workspace += 1;
        let workspace = Workspace {
            id,
            tiles,
            focus,
            fullscreen: None,
        };
        self.workspaces.insert(at, workspace);
        if at <= self.active {
            self.active += 1;
        }
    }

    fn activate(&mut self, index: usize) {
        let index = index.min(self.workspaces.len() - 1);
        if index != self.active {
            self.previous = Some(self.workspaces[self.active].id);
            self.active = index;
        }
    }

    fn previous_index(&self) -> usize {
        let previous = |ws: &Workspace| Some(ws.id) == self.previous;
        self.workspaces
            .iter()
            .position(previous)
            .unwrap_or(self.active)
    }

    /// A decision of the user, or of a client holding a valid token: the
    /// window gets the keyboard and the view goes to it.
    fn focus_window(&mut self, window: WindowId) {
        if let Some(home) = self.home(window) {
            self.workspaces[home].focus = Some(window);
            self.activate(home);
            self.overview = false;
        }
    }

    /// Moves a window to the front of the focus history.
    fn remember(&mut self, window: WindowId) {
        if let Some(state) = self.windows.get_mut(&window) {
            state.focused_before = true;
            self.mru.retain(|&other| other != window);
            self.mru.insert(0, window);
        }
    }

    fn apply(&mut self, event: Event, effects: &mut Vec<Effect>) {
        match event {
            Event::Map {
                window,
                app_id,
                parent,
                valid_token,
            } => {
                self.map(window, app_id, parent, valid_token, effects);
            }
            Event::Unmap(window) => self.unmap(window),
            Event::Activate {
                window,
                valid_token,
            } => {
                if valid_token {
                    self.focus_window(window);
                }
            }
            Event::FocusWindow(window) => self.focus_window(window),
            Event::Hover(window) => {
                // Hover never changes what is shown, so it reaches only
                // windows that are on screen at their place.
                let on_screen = !self.overview && self.switcher.is_none();
                if on_screen && self.home(window) == Some(self.active) {
                    self.workspaces[self.active].focus = Some(window);
                }
            }
            Event::FocusSettled(window) => {
                if self.focus() == Some(window) {
                    self.remember(window);
                }
            }
            Event::FocusLeft | Event::FocusRight => {
                let ws = &self.workspaces[self.active];
                let to = if event == Event::FocusLeft {
                    ws.tiles.first()
                } else {
                    ws.tiles.last()
                };
                let from = ws.focus.map(|focus| self.tile_of(focus));
                if let Some(&to) = to
                    && from != Some(to)
                {
                    self.workspaces[self.active].focus = self.recent_in(to);
                }
            }
            Event::WorkspaceUp => self.activate(self.active.saturating_sub(1)),
            Event::WorkspaceDown => self.activate(self.active + 1),
            Event::WorkspaceIndex(index) => {
                let index = index.min(self.workspaces.len() - 1);
                // niri's auto-back-and-forth, which the owner's profile enables.
                let back = index == self.active;
                self.activate(if back { self.previous_index() } else { index });
            }
            Event::WorkspacePrevious => self.activate(self.previous_index()),
            Event::MoveWindowUp | Event::MoveWindowDown | Event::Unsplit => {
                self.move_window(event != Event::MoveWindowUp, event == Event::Unsplit);
            }
            Event::SplitWith(window) => self.split_with(window),
            Event::SwapColumns => self.workspaces[self.active].tiles.reverse(),
            Event::ToggleFullscreen => {
                let tile = self.focus().map(|focus| self.tile_of(focus));
                let ws = &mut self.workspaces[self.active];
                ws.fullscreen = if ws.fullscreen == tile { None } else { tile };
            }
            Event::Close => effects.extend(self.focus().map(Effect::Close)),
            Event::OverviewOpen => self.overview = true,
            Event::OverviewClose => self.overview = false,
            Event::OverviewToggle => self.overview = !self.overview,
            Event::SwitcherStep { same_app, reverse } => self.switcher_step(same_app, reverse),
            Event::SwitcherCommit => {
                if let Some(switcher) = self.switcher.take() {
                    self.focus_window(switcher.selected);
                    self.remember(switcher.selected);
                }
            }
            Event::SwitcherCancel => self.switcher = None,
            Event::Output { size, reserved_top } => self.set_output(size, reserved_top),
        }
    }

    fn map(
        &mut self,
        window: WindowId,
        app_id: Option<String>,
        parent: Option<WindowId>,
        valid_token: bool,
        effects: &mut Vec<Effect>,
    ) {
        if self.windows.contains_key(&window) {
            return;
        }
        if self.windows.len() >= MAX_WINDOWS {
            effects.push(Effect::Close(window));
            return;
        }
        let home = parent.and_then(|parent| self.home(parent));
        let state = Window {
            app_id: app_id.filter(|id| id.len() <= MAX_APP_ID_LEN),
            parent: home.and(parent),
            focused_before: false,
        };
        self.windows.insert(window, state);
        self.mru.push(window);
        let home = if let Some(home) = home {
            let ws = &mut self.workspaces[home];
            // A dialog takes the keyboard from its own parent and from no
            // other window.
            if valid_token || ws.focus == parent {
                ws.focus = Some(window);
            }
            home
        } else {
            self.insert_workspace(self.active + 1, vec![window], Some(window));
            self.active + 1
        };
        if valid_token {
            self.activate(home);
        }
    }

    fn unmap(&mut self, window: WindowId) {
        let Some(home) = self.home(window) else {
            return;
        };
        if self
            .switcher
            .as_ref()
            .is_some_and(|switcher| switcher.selected == window)
        {
            self.switcher_advance(false);
            // It was the only candidate: nothing is left to choose from.
            self.switcher
                .take_if(|switcher| switcher.selected == window);
        }
        let Some(gone) = self.windows.remove(&window) else {
            return;
        };
        self.mru.retain(|&other| other != window);

        // Its dialogs go to its parent. A tile has none: its first dialog
        // takes the slot and the others follow that one.
        let orphan =
            |(id, state): (&WindowId, &Window)| (state.parent == Some(window)).then_some(*id);
        let orphans: Vec<WindowId> = self.windows.iter().filter_map(orphan).collect();
        let heir = gone.parent.or(orphans.first().copied());
        for orphan in orphans {
            if let Some(state) = self.windows.get_mut(&orphan) {
                state.parent = heir.filter(|&heir| heir != orphan);
            }
        }
        let ws = &mut self.workspaces[home];
        if gone.parent.is_none() {
            let replace = |&tile: &WindowId| if tile == window { heir } else { Some(tile) };
            ws.tiles = ws.tiles.iter().filter_map(replace).collect();
        }
        if ws.focus == Some(window) {
            ws.focus = gone.parent;
        }
    }

    /// Moves the focused column out of a pair into a workspace of its own,
    /// below or above. A lone window instead trades places with the
    /// neighboring workspace, unless the command only unsplits.
    fn move_window(&mut self, down: bool, pair_only: bool) {
        let Some(focus) = self.focus() else {
            return;
        };
        let tile = self.tile_of(focus);
        let ws = &mut self.workspaces[self.active];
        if ws.tiles.len() == 2 {
            ws.tiles.retain(|&other| other != tile);
            let at = self.active + usize::from(down);
            self.insert_workspace(at, vec![tile], Some(focus));
            self.activate(at);
        } else if !pair_only {
            let to = if down {
                Some(self.active + 1)
            } else {
                self.active.checked_sub(1)
            };
            // The last workspace is the empty one; it stays last.
            if let Some(to) = to.filter(|to| to + 1 < self.workspaces.len()) {
                self.workspaces.swap(self.active, to);
                self.active = to;
            }
        }
    }

    fn split_with(&mut self, window: WindowId) {
        let tile = self.tile_of(window);
        let from = self.home(tile).filter(|&from| from != self.active);
        if let Some(from) = from
            && self.workspaces[self.active].tiles.len() < 2
        {
            self.workspaces[from].tiles.retain(|&other| other != tile);
            let ws = &mut self.workspaces[self.active];
            ws.tiles.push(tile);
            // The user asked to see two windows.
            ws.fullscreen = None;
        }
    }

    fn switcher_step(&mut self, same_app: bool, reverse: bool) {
        let opening = self.switcher.is_none();
        if opening {
            // The window in use counts as used, however briefly.
            if let Some(focus) = self.focus() {
                self.remember(focus);
            }
            let first = self.mru.first().copied();
            self.switcher = first.map(|selected| Switcher {
                selected,
                app_id: None,
            });
        }
        let Some(switcher) = &mut self.switcher else {
            return;
        };
        // Alt+grave filters by the application of the selection at that
        // moment and keeps that filter; Alt+Tab drops it.
        let selected = self.windows.get(&switcher.selected);
        let own = || selected.and_then(|window| window.app_id.clone());
        switcher.app_id = if same_app {
            switcher.app_id.take().or_else(own)
        } else {
            None
        };
        // With nothing focused the most recent window is the first choice
        // itself, as in niri.
        if !(opening && !reverse && self.focus().is_none()) {
            self.switcher_advance(reverse);
        }
    }

    fn switcher_advance(&mut self, reverse: bool) {
        let Some(switcher) = &self.switcher else {
            return;
        };
        let list = self.candidates(switcher);
        let at = list.iter().position(|&window| window == switcher.selected);
        let next = at.map(|at| list[(at + if reverse { list.len() - 1 } else { 1 }) % list.len()]);
        if let (Some(switcher), Some(next)) = (&mut self.switcher, next) {
            switcher.selected = next;
        }
    }

    /// Restores what every event arm may leave open: the remembered focus of
    /// each workspace, the fullscreen rule and the empty-workspace rule.
    fn settle(&mut self) {
        for index in 0..self.workspaces.len() {
            let here = |window: &WindowId| self.home(*window) == Some(index);
            let focus = self.workspaces[index].focus.filter(here);
            let focus = focus.or_else(|| self.mru.iter().copied().find(here));
            let tile = focus.map(|focus| self.tile_of(focus));
            let ws = &mut self.workspaces[index];
            ws.focus = focus;
            // Fullscreen ends when its tile goes or the focus moves to the
            // sibling, which would otherwise be focused and invisible.
            ws.fullscreen = ws.fullscreen.filter(|&fullscreen| Some(fullscreen) == tile);
        }

        let (active, last) = (self.workspaces[self.active].id, self.workspaces.len() - 1);
        let mut index = 0..;
        self.workspaces.retain(|ws| {
            let last = index.next() == Some(last);
            last || ws.id == active || !ws.tiles.is_empty()
        });
        let active = |ws: &Workspace| ws.id == active;
        self.active = self.workspaces.iter().position(active).unwrap_or(0);
        if self.workspaces.last().is_none_or(|ws| !ws.tiles.is_empty()) {
            self.insert_workspace(self.workspaces.len(), Vec::new(), None);
        }

        if let Some(focus) = self.focus()
            && self
                .windows
                .get(&focus)
                .is_some_and(|window| !window.focused_before)
        {
            self.remember(focus);
        }
    }
}
