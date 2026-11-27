//! The drawer menu: levels, rows, navigation and geometry (DESIGN §6.1, §7.2).
//!
//! The drawer never touches the machine, a dialog or the process. It records a [`Request`] and the
//! app performs it, which is what makes every interaction testable without a window. Rows come
//! from the modules (§6.1): [`Rows::collect`] asks each one once a frame, in a fixed order, and
//! filters the result through the running game's capabilities ([`crate::caps`]).

use crate::app::App;
use crate::caps::{self, RowGate};
use crate::recents::Recent;
use crate::settings::Direction;

/// Row height, normal mode.
pub const ROW_H: f32 = 22.0;
/// Row height, compact mode (a drawer narrower than 320 px).
pub const ROW_H_COMPACT: f32 = 20.0;
/// Gap between two module groups.
pub const GAP_H: f32 = 8.0;
/// The header's height: the parent line.
pub const HEADER_H: f32 = 34.0;
/// The footer's height: the two hint lines.
pub const FOOTER_H: f32 = 34.0;
/// Left inset of a row's label.
pub const ROW_X: f32 = 14.0;
/// Right inset of a row's value and fill.
pub const ROW_INSET: f32 = 26.0;
/// Left edge of a row's fill.
pub const ROW_FILL_X: f32 = 4.0;
/// The row fill stops this far short of the drawer's width.
pub const ROW_FILL_INSET: f32 = 16.0;
/// The left chevron's zone: a click in the first `CHEVRON_W` px of a choice row steps it back.
pub const CHEVRON_W: f32 = 24.0;

/// One page of the drawer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Level {
    #[default]
    Top,
    RecentRoms,
    Game,
    Video,
    Audio,
    Input,
    Emulation,
}

impl Level {
    pub const ALL: [Level; 7] = [
        Level::Top,
        Level::RecentRoms,
        Level::Game,
        Level::Video,
        Level::Audio,
        Level::Input,
        Level::Emulation,
    ];

    /// The header's name for the page.
    pub fn title(self) -> &'static str {
        match self {
            Level::Top => "Main menu",
            Level::RecentRoms => "Recent ROMs",
            Level::Game => "Game",
            Level::Video => "Video",
            Level::Audio => "Audio",
            Level::Input => "Input",
            Level::Emulation => "Emulation",
        }
    }

    /// The path of pages down to this one, `Main menu` first.
    pub fn breadcrumb(self) -> Vec<&'static str> {
        match self {
            Level::Top => vec!["Main menu"],
            other => vec!["Main menu", other.title()],
        }
    }

    /// The page a section row opens, for the sections that have one.
    pub fn section(self) -> Option<Section> {
        match self {
            Level::Game => Some(Section::Game),
            Level::Video => Some(Section::Video),
            Level::Audio => Some(Section::Audio),
            Level::Input => Some(Section::Input),
            Level::Emulation => Some(Section::Emulation),
            Level::Top | Level::RecentRoms => None,
        }
    }
}

/// The five groups of settings rows the drawer shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Game,
    Video,
    Audio,
    Input,
    Emulation,
}

impl Section {
    pub const ALL: [Section; 5] = [
        Section::Game,
        Section::Video,
        Section::Audio,
        Section::Input,
        Section::Emulation,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Section::Game => "Game",
            Section::Video => "Video",
            Section::Audio => "Audio",
            Section::Input => "Input",
            Section::Emulation => "Emulation",
        }
    }

    pub fn level(self) -> Level {
        match self {
            Section::Game => Level::Game,
            Section::Video => Level::Video,
            Section::Audio => Level::Audio,
            Section::Input => Level::Input,
            Section::Emulation => Level::Emulation,
        }
    }
}

/// How a row behaves when it is entered or stepped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKind {
    /// Enter runs it.
    Action,
    /// Left/Right pick among more than two values.
    Choice,
    /// Left/Right, or Enter, flip two values.
    Toggle,
    /// Enter opens a file dialog.
    Pick,
    /// Enter asks for the next key pressed.
    Rebind,
    /// Reports a value and cannot be chosen.
    Info,
}

/// One row of the drawer, as the module that owns it describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub label: String,
    /// The value column's text, already resolved by the owner: the drawer never reads settings.
    pub value: String,
    pub kind: RowKind,
    pub desc: &'static str,
    pub hotkey: &'static str,
    /// The capability the running game must report for the row to be drawn.
    pub requires: Option<tendy_core::Capability>,
    /// What the drawer itself does with the row. `None` for a module's row: it goes to the owner.
    pub act: Option<Request>,
}

impl Row {
    /// A row that runs something when entered.
    pub fn action(
        label: impl Into<String>,
        desc: &'static str,
        hotkey: &'static str,
        request: Request,
    ) -> Row {
        Row {
            label: label.into(),
            value: String::new(),
            kind: RowKind::Action,
            desc,
            hotkey,
            requires: None,
            act: Some(request),
        }
    }

    /// A row that only reports.
    pub fn info(label: impl Into<String>, value: impl Into<String>, desc: &'static str) -> Row {
        Row {
            label: label.into(),
            value: value.into(),
            kind: RowKind::Info,
            desc,
            hotkey: "",
            requires: None,
            act: None,
        }
    }

    /// The same row gated on a capability.
    pub fn requiring(mut self, capability: tendy_core::Capability) -> Row {
        self.requires = Some(capability);
        self
    }
}

/// A module of §6.1 that may add rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Module {
    /// The drawer's own rows: open, recents, sections, session commands.
    Menu,
    Caps,
    Keys,
    Gamepad,
    Display,
    Audio,
    Switch,
    Config,
    Settings,
    Paths,
    Games,
    States,
    Thumb,
    FirstRun,
    Rewind,
    Screenshot,
    Record,
    Avi,
}

impl Module {
    /// The order the drawer collects rows in. A module's rows land on the page its
    /// [`Module::level`] names, and inside a page in this order.
    pub const ROW_ORDER: [Module; 17] = [
        Module::Caps,
        Module::States,
        Module::Rewind,
        Module::Display,
        Module::Thumb,
        Module::Screenshot,
        Module::Record,
        Module::Avi,
        Module::Audio,
        Module::Keys,
        Module::Gamepad,
        Module::Switch,
        Module::Config,
        Module::Settings,
        Module::Paths,
        Module::Games,
        Module::FirstRun,
    ];

    /// The page this module's rows are drawn on.
    pub fn level(self) -> Level {
        match self {
            Module::Menu | Module::Caps | Module::States | Module::Rewind => Level::Game,
            Module::Display | Module::Thumb | Module::Screenshot | Module::Record | Module::Avi => {
                Level::Video
            }
            Module::Audio => Level::Audio,
            Module::Keys | Module::Gamepad => Level::Input,
            Module::Switch
            | Module::Config
            | Module::Settings
            | Module::Paths
            | Module::Games
            | Module::FirstRun => Level::Emulation,
        }
    }

    /// The heading drawn above this module's rows. `Menu` rows have none.
    pub fn heading(self) -> &'static str {
        match self {
            Module::Menu => "",
            Module::Caps => "CAPABILITIES",
            Module::Keys => "KEYS",
            Module::Gamepad => "GAMEPAD",
            Module::Display => "DISPLAY",
            Module::Audio => "AUDIO",
            Module::Switch => "SESSION",
            Module::Config => "CONFIG FILE",
            Module::Settings => "SETTINGS",
            Module::Paths => "FOLDERS",
            Module::Games => "THIS GAME",
            Module::States => "STATE",
            Module::Thumb => "SLOTS",
            Module::FirstRun => "FIRST RUN",
            Module::Rewind => "REWIND",
            Module::Screenshot => "SCREENSHOT",
            Module::Record => "RECORDING",
            Module::Avi => "AVI",
        }
    }
}

/// One module's rows, on the page they belong to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowGroup {
    pub module: Module,
    pub level: Level,
    pub rows: Vec<Row>,
}

/// Every module's rows for this frame.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Rows {
    groups: Vec<RowGroup>,
}

impl Rows {
    /// Asks every module once, in [`Module::ROW_ORDER`], and drops the rows the game cannot use.
    pub fn collect(app: &App) -> Rows {
        let capabilities = app.capabilities();
        let mut groups = Vec::new();
        for level in Level::ALL {
            groups.push(RowGroup {
                module: Module::Menu,
                level,
                rows: own_rows(app, level),
            });
        }
        for module in Module::ROW_ORDER {
            let rows = module_rows(app, module)
                .into_iter()
                .filter(|row| {
                    caps::visible(
                        &RowGate {
                            requires: row.requires,
                        },
                        capabilities,
                    )
                })
                .collect();
            groups.push(RowGroup {
                module,
                level: module.level(),
                rows,
            });
        }
        Rows { groups }
    }

    /// The rows of one module.
    pub fn of(&self, module: Module) -> &[Row] {
        self.groups
            .iter()
            .find(|group| group.module == module)
            .map_or(&[], |group| group.rows.as_slice())
    }

    /// The drawer's own rows for one page.
    pub fn own(&self, level: Level) -> &[Row] {
        self.groups
            .iter()
            .find(|group| group.module == Module::Menu && group.level == level)
            .map_or(&[], |group| group.rows.as_slice())
    }

    /// Every group, in collection order.
    pub fn groups(&self) -> &[RowGroup] {
        &self.groups
    }

    /// One row by reference.
    pub fn row(&self, at: Ref) -> Option<&Row> {
        self.of(at.module).get(at.index)
    }
}

/// The drawer's own rows for a page: everything that is not a module's setting.
fn own_rows(app: &App, level: Level) -> Vec<Row> {
    match level {
        Level::Top => {
            let mut rows = vec![Row::action(
                "Open ROM...",
                "Open a ROM or an archive",
                "Enter",
                Request::OpenRom,
            )];
            let recent = app.recents().len();
            rows.push(Row::action(
                "Recent ROMs",
                "The last ten ROMs opened",
                "Enter",
                Request::Enter(Level::RecentRoms),
            ));
            rows.last_mut().expect("just pushed").value = format!("{recent} >");
            for section in Section::ALL {
                rows.push(Row::action(
                    section.name(),
                    "Settings for this session",
                    "Enter",
                    Request::Enter(section.level()),
                ));
                rows.last_mut().expect("just pushed").value = ">".to_string();
            }
            rows.push(Row::action(
                "Quit",
                "Write the battery and close",
                "",
                Request::Quit,
            ));
            rows
        }
        Level::RecentRoms => {
            let mut rows = Vec::new();
            for (index, recent) in app.recents().iter().enumerate() {
                let name = recent
                    .path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| recent.path.to_string_lossy().into_owned());
                let mut row = Row::action(
                    "Recent ROM",
                    "Reopen this ROM",
                    "Enter",
                    Request::OpenRecent(index),
                );
                row.value = format!("{} — {name}", recent.system.display_name());
                if recent.gone {
                    row.desc = "The file is gone";
                }
                rows.push(row);
            }
            if !rows.is_empty() {
                rows.push(Row::action(
                    "Clear list",
                    "Forget every remembered ROM",
                    "",
                    Request::ClearRecents,
                ));
            }
            rows
        }
        Level::Game => {
            let mut rows = Vec::new();
            let label = if app.paused() { "Resume" } else { "Pause" };
            let request = if app.paused() {
                Request::Resume
            } else {
                Request::Pause
            };
            rows.push(Row::action(
                label,
                "Hold the game where it is",
                "P",
                request,
            ));
            rows.push(Row::action(
                "Reset",
                "Press the console's reset button",
                "",
                Request::Reset,
            ));
            rows
        }
        Level::Video | Level::Audio | Level::Input | Level::Emulation => Vec::new(),
    }
}

/// One module's rows, by asking the module.
fn module_rows(app: &App, module: Module) -> Vec<Row> {
    match module {
        Module::Menu => Vec::new(),
        Module::Caps => crate::caps::rows(app),
        Module::Keys => crate::keys::rows(app),
        Module::Gamepad => crate::gamepad::rows(app),
        Module::Display => crate::display::rows(app),
        Module::Audio => crate::audio::rows(app),
        Module::Switch => crate::switch::rows(app),
        Module::Config => crate::config::rows(app),
        Module::Settings => crate::settings::rows(app),
        Module::Paths => crate::paths::rows(app),
        Module::Games => crate::games::rows(app),
        Module::States => crate::states::rows(app),
        Module::Thumb => crate::thumb::rows(app),
        Module::FirstRun => crate::first_run::rows(app),
        Module::Rewind => crate::rewind::rows(app),
        Module::Screenshot => crate::screenshot::rows(app),
        Module::Record => crate::record::rows(app),
        Module::Avi => crate::avi::rows(app),
    }
}

/// What was done to a module's row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowAct {
    /// Enter, or a click on the row.
    Activate,
    /// Left or Right on a choice row.
    Step(Direction),
    /// A key, while a rebind row waited for one.
    Key(String),
}

/// One module's row, by module and index in that module's list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ref {
    pub module: Module,
    pub index: usize,
}

/// Something the drawer wants done. The app performs it; the drawer never does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// The file dialog, then `App::open`.
    OpenRom,
    /// Reopen the `n`th recent ROM.
    OpenRecent(usize),
    ClearRecents,
    Enter(Level),
    Resume,
    Pause,
    Reset,
    Quit,
    /// A row a module produced.
    Row(Ref, RowAct),
}

/// What a row does when it is activated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ItemAct {
    /// A row the drawer owns.
    Own(Request),
    /// A row a module owns.
    Module(Ref),
}

/// A selectable row inside the drawer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub row: Row,
    pub act: ItemAct,
    pub enabled: bool,
}

/// One drawn line of a page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    Heading(&'static str),
    Item(Item),
    Gap,
}

/// Where one entry lands: its index in the page, its top and its height.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Slot {
    pub index: usize,
    pub y: f32,
    pub h: f32,
    /// The row is a `< a | b >` choice, which draws chevrons when it is selected.
    pub arrows: bool,
    /// The row can take the cursor. A heading or a gap is drawn in a slot too, and cannot.
    pub item: bool,
}

/// The geometry a page is drawn with.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layout {
    pub dw: f32,
    pub compact: bool,
    pub size: f32,
    pub list_top: f32,
    pub list_bottom: f32,
    pub slots: Vec<Slot>,
}

/// The drawer's state.
#[derive(Clone, Debug, Default)]
pub struct Menu {
    open: bool,
    level: Level,
    cursor: usize,
    first: usize,
    viewport: usize,
    capture: Option<Ref>,
    request: Option<Request>,
    rows: Rows,
    recents: Vec<Recent>,
}

impl Menu {
    pub fn new() -> Menu {
        Menu::default()
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Opens the drawer at the top level, with the cursor on the first row that can be chosen.
    pub fn open(&mut self) {
        self.open = true;
        self.level = Level::Top;
        self.first = 0;
        self.capture = None;
        self.request = None;
        self.cursor = self.first_selectable();
    }

    pub fn close(&mut self) {
        self.open = false;
        self.capture = None;
    }

    pub fn level(&self) -> Level {
        self.level
    }

    /// Goes to `level`, with the cursor on the first row that can be chosen.
    pub fn set_level(&mut self, level: Level) {
        self.level = level;
        self.first = 0;
        self.cursor = self.first_selectable();
    }

    /// Toggles the drawer: what the menu key does.
    pub fn toggle(&mut self) {
        if self.open {
            self.close();
        } else {
            self.open();
        }
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The first entry the list is scrolled to.
    pub fn first(&self) -> usize {
        self.first
    }

    /// The entries of the current page.
    pub fn entries(&self) -> Vec<Entry> {
        self.entries_at(self.level)
    }

    /// The rows the drawer holds this frame.
    pub fn rows(&self) -> &Rows {
        &self.rows
    }

    pub fn set_rows(&mut self, rows: Rows) {
        self.rows = rows;
        self.clamp_cursor();
    }

    pub fn recents(&self) -> &[Recent] {
        &self.recents
    }

    pub fn set_recents(&mut self, recents: Vec<Recent>) {
        self.recents = recents;
        self.clamp_cursor();
    }

    /// The row waiting for a key, while a rebind captures one.
    pub fn capture(&self) -> Option<Ref> {
        self.capture
    }

    /// The row under the cursor, when the drawer is open and the cursor is on one.
    pub fn item(&self) -> Option<Item> {
        if !self.open {
            return None;
        }
        match self.entries().get(self.cursor) {
            Some(Entry::Item(item)) => Some(item.clone()),
            _ => None,
        }
    }

    /// Takes the request the last activation recorded.
    pub fn take_request(&mut self) -> Option<Request> {
        self.request.take()
    }

    /// How many rows fit the list box; the window loop sets this before it navigates.
    pub fn set_viewport(&mut self, rows: usize) {
        self.viewport = rows.max(1);
        self.scroll_into_view();
    }

    /// The visible entry range, with the cursor always inside it.
    pub fn visible(&self, rows: usize) -> std::ops::Range<usize> {
        let rows = rows.max(1);
        let first = self.first.min(self.cursor);
        first..(first + rows)
    }

    /// Moves the cursor by `delta` selectable rows, wrapping at both ends.
    pub fn move_by(&mut self, delta: isize) {
        let selectable: Vec<usize> = self
            .entries()
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| match entry {
                Entry::Item(item) if item.enabled => Some(index),
                _ => None,
            })
            .collect();
        if selectable.is_empty() {
            return;
        }
        let at = selectable
            .iter()
            .position(|index| *index == self.cursor)
            .unwrap_or(0);
        let len = selectable.len() as isize;
        let next = (at as isize + delta).rem_euclid(len);
        self.cursor = selectable[next as usize];
        self.scroll_into_view();
    }

    /// Puts the cursor on entry `index` when it can be chosen. True when the cursor moved.
    pub fn select(&mut self, index: usize) -> bool {
        let choosable = matches!(
            self.entries().get(index),
            Some(Entry::Item(item)) if item.enabled
        );
        if !choosable || self.cursor == index {
            return false;
        }
        self.cursor = index;
        self.scroll_into_view();
        true
    }

    /// Arms the capture for one rebind row.
    pub fn arm_capture(&mut self, at: Ref) {
        self.capture = Some(at);
    }

    fn entries_at(&self, level: Level) -> Vec<Entry> {
        if level == Level::RecentRoms {
            return recent_entries(self);
        }
        let mut out: Vec<Entry> = Vec::new();
        for row in self.rows.own(level) {
            out.push(Entry::Item(Item {
                act: row
                    .act
                    .clone()
                    .map_or(ItemAct::Own(Request::OpenRom), ItemAct::Own),
                row: row.clone(),
                enabled: row.kind != RowKind::Info,
            }));
        }
        let mut grouped = false;
        for group in self.rows.groups() {
            if group.module == Module::Menu || group.level != level || group.rows.is_empty() {
                continue;
            }
            if grouped {
                out.push(Entry::Gap);
            }
            out.push(Entry::Heading(group.module.heading()));
            for (index, row) in group.rows.iter().enumerate() {
                out.push(Entry::Item(Item {
                    row: row.clone(),
                    act: ItemAct::Module(Ref {
                        module: group.module,
                        index,
                    }),
                    enabled: row.kind != RowKind::Info,
                }));
            }
            grouped = true;
        }
        if !out.iter().any(|entry| matches!(entry, Entry::Item(_))) {
            out.push(Entry::Item(Item {
                row: Row::info(
                    "No settings here yet",
                    "",
                    "This section is filled in by a later update",
                ),
                act: ItemAct::Own(Request::Enter(Level::Top)),
                enabled: false,
            }));
        }
        out
    }

    /// The first entry that can be chosen, or 0.
    fn first_selectable(&self) -> usize {
        self.entries()
            .iter()
            .position(|entry| matches!(entry, Entry::Item(item) if item.enabled))
            .unwrap_or(0)
    }

    /// Keeps the cursor on a row that can be chosen after the rows or the page changed.
    fn clamp_cursor(&mut self) {
        let entries = self.entries();
        let ok = matches!(entries.get(self.cursor), Some(Entry::Item(item)) if item.enabled);
        if !ok {
            self.cursor = entries
                .iter()
                .position(|entry| matches!(entry, Entry::Item(item) if item.enabled))
                .unwrap_or(0);
            self.first = 0;
        }
        self.scroll_into_view();
    }

    /// Keeps the cursor inside the viewport, moving `first` forward by whole entries.
    fn scroll_into_view(&mut self) {
        let rows = self.viewport.max(1);
        if self.cursor < self.first {
            self.first = self.cursor;
        } else {
            while self.cursor >= self.first + rows {
                self.first += 1;
            }
        }
    }
}

/// The recents page: one row per remembered ROM, then the clear row.
fn recent_entries(menu: &Menu) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    let mut any = false;
    for (index, recent) in menu.recents.iter().enumerate() {
        let name = recent
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| recent.path.to_string_lossy().into_owned());
        let mut row = Row::action("", "", "Enter", Request::OpenRecent(index));
        row.label = format!("{} — {name}", recent.system.display_name());
        row.value = if recent.gone {
            "gone".to_string()
        } else {
            ">".to_string()
        };
        row.desc = if recent.gone {
            "The file is gone"
        } else {
            "Reopen this ROM"
        };
        out.push(Entry::Item(Item {
            row,
            act: ItemAct::Own(Request::OpenRecent(index)),
            enabled: !recent.gone,
        }));
        any = true;
    }
    if any {
        out.push(Entry::Gap);
        out.push(Entry::Item(Item {
            row: Row::action(
                "Clear list",
                "Forget every remembered ROM",
                "",
                Request::ClearRecents,
            ),
            act: ItemAct::Own(Request::ClearRecents),
            enabled: true,
        }));
    } else {
        out.push(Entry::Item(Item {
            row: Row::info(
                "No recent ROMs yet",
                "",
                "Open a ROM and it is remembered here",
            ),
            act: ItemAct::Own(Request::Enter(Level::Top)),
            enabled: false,
        }));
    }
    out
}

/// A navigation key: Up/Down move, Left/Right step a choice row, Enter activates, Esc goes up a
/// level and then closes the drawer. Returns whether the key was consumed.
pub fn key(menu: &mut Menu, name: &str) -> bool {
    if let Some(at) = menu.capture {
        menu.capture = None;
        if name == "Escape" {
            return true;
        }
        menu.request = Some(Request::Row(at, RowAct::Key(name.to_string())));
        return true;
    }
    match name {
        "Up" => {
            menu.move_by(-1);
            true
        }
        "Down" => {
            menu.move_by(1);
            true
        }
        "Left" => step(menu, Direction::Left),
        "Right" => step(menu, Direction::Right),
        "Enter" => enter(menu),
        "Escape" => esc(menu),
        _ => false,
    }
}

/// Steps the row under the cursor, when an arrow changes it.
pub fn step(menu: &mut Menu, direction: Direction) -> bool {
    let Some(item) = menu.item() else {
        return false;
    };
    if !item.enabled {
        return false;
    }
    match (&item.act, item.row.kind) {
        (ItemAct::Module(at), RowKind::Choice | RowKind::Toggle) => {
            menu.request = Some(Request::Row(*at, RowAct::Step(direction)));
            true
        }
        _ => false,
    }
}

/// Enter on the row under the cursor. Settings are left as a [`Request`]: the drawer never touches
/// the machine, a dialog or the process.
pub fn enter(menu: &mut Menu) -> bool {
    let Some(item) = menu.item() else {
        return false;
    };
    if !item.enabled {
        return false;
    }
    match item.act {
        ItemAct::Own(request) => {
            menu.request = Some(request);
            true
        }
        ItemAct::Module(at) => {
            let act = match item.row.kind {
                RowKind::Rebind => {
                    menu.arm_capture(at);
                    return true;
                }
                RowKind::Toggle | RowKind::Choice => RowAct::Step(Direction::Right),
                RowKind::Action | RowKind::Pick | RowKind::Info => RowAct::Activate,
            };
            menu.request = Some(Request::Row(at, act));
            true
        }
    }
}

/// The mouse's activation of entry `index`, clicked at `x`: the cursor goes there first, then the
/// row does what Enter does on it — except a click inside a choice row's left chevron zone, which
/// steps it back like Left.
pub fn click(menu: &mut Menu, index: usize, x: f32) -> bool {
    menu.select(index);
    if menu.cursor() != index {
        return false;
    }
    if x < ROW_FILL_X + CHEVRON_W && step(menu, Direction::Left) {
        return true;
    }
    enter(menu)
}

/// Esc goes up one level, then closes the drawer. Returns whether the drawer is still open.
pub fn esc(menu: &mut Menu) -> bool {
    if menu.level == Level::Top {
        menu.close();
        false
    } else {
        menu.set_level(Level::Top);
        true
    }
}

/// Wraps a description to `chars` characters a line, breaking on spaces; a word longer than the
/// line is cut, so the footer always fits.
pub fn wrap_desc(text: &str, chars: usize) -> Vec<String> {
    let chars = chars.max(1);
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if line.is_empty() {
            line.push_str(word);
        } else if line.chars().count() + 1 + word.chars().count() <= chars {
            line.push(' ');
            line.push_str(word);
        } else {
            lines.push(std::mem::take(&mut line));
            line.push_str(word);
        }
        while line.chars().count() > chars {
            let cut = line
                .char_indices()
                .nth(chars)
                .map_or(line.len(), |(at, _)| at);
            lines.push(line[..cut].to_string());
            line = line[cut..].to_string();
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// The header's parent line: at the top level the running ROM's file name, or `No game loaded`;
/// below it the pages above this one.
pub fn parent_line(level: Level, rom_name: &str) -> String {
    if level != Level::Top {
        let crumbs = level.breadcrumb();
        return crumbs[..crumbs.len() - 1].join(" > ");
    }
    if rom_name.is_empty() {
        "No game loaded".to_string()
    } else {
        rom_name.to_string()
    }
}

/// The two footer hint lines.
pub fn hints(level: Level, capturing: bool) -> [&'static str; 2] {
    if capturing {
        return ["Rebind: press the key", "Esc cancels"];
    }
    match level {
        Level::Top => ["Up/Down move  Enter open", "Esc close"],
        Level::RecentRoms => ["Up/Down move  Enter open", "Esc back"],
        _ => ["Up/Down move  Left/Right change", "Enter select  Esc back"],
    }
}

/// The description wraps to `floor((dw - 30) / 7)` characters, so a line is never wider than the
/// drawer it is drawn in.
pub fn desc_chars(dw: f32) -> usize {
    (((dw - 30.0) / 7.0) as usize).max(1)
}

/// `win_w < 512` -> `win_w`; `512..=767` -> 288; `>= 768` -> 320.
pub fn drawer_width(win_w: f32) -> f32 {
    if win_w < 512.0 {
        win_w
    } else if win_w < 768.0 {
        288.0
    } else {
        320.0
    }
}

/// Whether the drawer takes the whole window.
pub fn is_compact(win_w: f32) -> bool {
    win_w < DRAWER_MIN_W
}

/// The narrowest window the drawer keeps its own width in.
pub const DRAWER_MIN_W: f32 = 512.0;

/// Where the page's entries land. `first` is the entry the list is scrolled to; a slot that would
/// end below the list box is left out, so the painter never has to clip.
pub fn layout(dw: f32, win_h: f32, entries: &[Entry], first: usize) -> Layout {
    let compact = dw < 320.0;
    let row_h = if compact { ROW_H_COMPACT } else { ROW_H };
    let size = if compact { 14.0 } else { 16.0 };
    let list_top = HEADER_H;
    let list_bottom = (win_h - FOOTER_H).max(list_top);
    let mut y = list_top;
    let mut slots = Vec::new();
    for (index, entry) in entries.iter().enumerate().skip(first) {
        if y >= list_bottom {
            break;
        }
        let h = match entry {
            Entry::Gap => GAP_H,
            Entry::Heading(_) | Entry::Item(_) => row_h,
        };
        if y + h <= list_bottom {
            slots.push(Slot {
                index,
                y,
                h,
                arrows: matches!(entry, Entry::Item(item) if matches!(item.row.kind, RowKind::Choice | RowKind::Toggle)),
                item: matches!(entry, Entry::Item(_)),
            });
        }
        y += h;
    }
    Layout {
        dw,
        compact,
        size,
        list_top,
        list_bottom,
        slots,
    }
}

/// The entry index under a click at `(x, y)`, when it is a row that can be chosen.
pub fn hit(layout: &Layout, x: f32, y: f32) -> Option<usize> {
    if x < ROW_FILL_X || x > layout.dw - ROW_FILL_INSET {
        return None;
    }
    layout
        .slots
        .iter()
        .find(|slot| slot.item && y >= slot.y && y < slot.y + slot.h)
        .map(|slot| slot.index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::Paths;

    fn scratch(name: &str) -> Paths {
        Paths::new(
            std::env::temp_dir().join(format!("tendyemu-menu-{name}-{}", std::process::id())),
        )
    }

    fn app() -> App {
        App::new(scratch("rows"))
    }

    fn refreshed(app: &App) -> Menu {
        let mut menu = Menu::new();
        menu.set_rows(Rows::collect(app));
        menu
    }

    #[test]
    fn a_fresh_menu_is_closed_and_empty() {
        let menu = Menu::new();
        assert!(!menu.is_open());
        assert_eq!(menu.level(), Level::Top);
        assert!(menu.item().is_none());
    }

    #[test]
    fn opening_lands_on_open_rom_and_escape_closes() {
        let app = app();
        let mut menu = refreshed(&app);
        menu.open();
        assert!(menu.is_open());
        let item = menu.item().expect("the cursor starts on a row");
        assert_eq!(item.row.label, "Open ROM...");
        assert!(!esc(&mut menu));
        assert!(!menu.is_open());
    }

    #[test]
    fn a_section_row_opens_and_escape_goes_back_up() {
        let app = app();
        let mut menu = refreshed(&app);
        menu.open();
        // Open ROM, Recent ROMs, then Game.
        menu.select(2);
        assert!(enter(&mut menu));
        assert_eq!(menu.take_request(), Some(Request::Enter(Level::Game)));
        menu.set_level(Level::Game);
        assert_eq!(menu.level(), Level::Game);
        assert!(esc(&mut menu));
        assert_eq!(menu.level(), Level::Top);
        assert!(menu.is_open());
    }

    #[test]
    fn moving_wraps_at_both_ends() {
        let app = app();
        let mut menu = refreshed(&app);
        menu.open();
        let count = menu
            .entries()
            .iter()
            .filter(|entry| matches!(entry, Entry::Item(item) if item.enabled))
            .count();
        assert!(count > 2);
        for _ in 0..count {
            menu.move_by(1);
        }
        let start = menu.cursor();
        menu.move_by(1);
        assert_ne!(menu.cursor(), start, "the last row wraps to the first");
        menu.move_by(-1);
        assert_eq!(menu.cursor(), start, "and the first wraps back");
    }

    #[test]
    fn the_cursor_never_lands_on_a_heading_or_a_gap() {
        let app = app();
        let mut menu = refreshed(&app);
        menu.open();
        let entries = menu.entries();
        for _ in 0..entries.len() * 2 {
            assert!(
                matches!(entries.get(menu.cursor()), Some(Entry::Item(item)) if item.enabled),
                "the cursor sits on a row that cannot be chosen"
            );
            menu.move_by(1);
        }
    }

    #[test]
    fn the_cursor_stays_inside_the_viewport() {
        let app = app();
        let mut menu = refreshed(&app);
        menu.open();
        menu.set_viewport(3);
        for _ in 0..10 {
            menu.move_by(1);
            let visible = menu.visible(3);
            assert!(visible.contains(&menu.cursor()));
        }
    }

    #[test]
    fn an_empty_section_says_so_instead_of_showing_nothing() {
        let app = app();
        let mut menu = refreshed(&app);
        menu.set_level(Level::Audio);
        let entries = menu.entries();
        assert_eq!(entries.len(), 1);
        assert!(matches!(entries[0], Entry::Item(_)));
        assert!(menu.item().is_none() || !menu.item().unwrap().enabled);
    }

    #[test]
    fn a_capture_takes_the_next_key_and_escape_cancels() {
        let app = app();
        let mut menu = refreshed(&app);
        menu.open();
        let at = Ref {
            module: Module::Keys,
            index: 0,
        };
        menu.arm_capture(at);
        assert!(key(&mut menu, "Escape"));
        assert_eq!(menu.capture(), None);
        assert_eq!(menu.take_request(), None);
        menu.arm_capture(at);
        assert!(key(&mut menu, "F5"));
        assert_eq!(
            menu.take_request(),
            Some(Request::Row(at, RowAct::Key("F5".to_string())))
        );
    }

    #[test]
    fn a_click_outside_a_row_does_nothing() {
        let app = app();
        let mut menu = refreshed(&app);
        menu.open();
        let entries = menu.entries();
        let layout = layout(320.0, 600.0, &entries, menu.first());
        assert_eq!(hit(&layout, 1.0, layout.list_top + 1.0), None);
        let first = layout.slots.iter().find(|slot| slot.item).unwrap();
        assert_eq!(hit(&layout, 60.0, first.y + 2.0), Some(first.index));
    }

    #[test]
    fn a_description_wraps_on_spaces() {
        let lines = wrap_desc("Open a ROM or an archive", 12);
        assert_eq!(lines, vec!["Open a ROM", "or an", "archive"]);
        assert!(
            wrap_desc("averylongwordthatdoesnotfit", 5)
                .iter()
                .all(|line| line.chars().count() <= 5)
        );
    }

    #[test]
    fn the_drawer_width_follows_the_window() {
        assert_eq!(drawer_width(400.0), 400.0);
        assert_eq!(drawer_width(600.0), 288.0);
        assert_eq!(drawer_width(1024.0), 320.0);
        assert!(is_compact(400.0));
        assert!(!is_compact(800.0));
    }

    #[test]
    fn the_header_says_where_the_page_is() {
        assert_eq!(parent_line(Level::Top, ""), "No game loaded");
        assert_eq!(parent_line(Level::Top, "Zelda.gb"), "Zelda.gb");
        assert_eq!(parent_line(Level::Game, "Zelda.gb"), "Main menu");
    }
}
