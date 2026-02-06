use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use owo_colors::OwoColorize;
use zellij_tile::prelude::*;

// ----------------------------------- Debug Logging -----------------------------------

#[cfg(debug_assertions)]
macro_rules! debug_log {
    ($($arg:tt)*) => {{
        let msg = format!($($arg)*);
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let line = format!("[{timestamp}] {msg}\n");
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("/host/harpoon_debug.log")
            .and_then(|mut f| std::io::Write::write_all(&mut f, line.as_bytes()));
    }};
}

#[cfg(not(debug_assertions))]
macro_rules! debug_log {
    ($($arg:tt)*) => {};
}

// ----------------------------------- Slot Data Structures -----------------------------------

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct HarpoonSlot {
    pub session_name: String,
    pub tab_name: String,
    pub tab_position: usize,
    pub pane_id: u32,
    pub pane_title: String,
}

#[derive(Default, Serialize, Deserialize, Debug, Clone)]
pub struct HarpoonData {
    pub slots: BTreeMap<char, HarpoonSlot>,
}

// ----------------------------------- Legacy Pane Display -----------------------------------

#[derive(Clone, Serialize, Deserialize)]
pub struct Pane {
    pub pane_info: PaneInfo,
    pub tab_info: TabInfo,
}

// ----------------------------------- Pending Actions -----------------------------------

enum PendingAction {
    Jump(char),
    Assign(char),
}

// ----------------------------------- Helper Functions -----------------------------------

fn get_focused_tab(tab_infos: &Vec<TabInfo>) -> Option<TabInfo> {
    tab_infos.iter().find(|tab| tab.active).cloned()
}

fn get_focused_pane(tab_position: usize, pane_manifest: &PaneManifest) -> Option<PaneInfo> {
    let panes = pane_manifest.panes.get(&tab_position)?;
    panes
        .iter()
        .find(|pane| pane.is_focused && !pane.is_plugin)
        .cloned()
}

fn is_slot_key(c: char) -> bool {
    match c {
        'a'..='z' if c != 'j' && c != 'k' => true,
        '0'..='9' => true,
        _ => false,
    }
}

// ----------------------------------- State -----------------------------------

#[derive(Default)]
struct State {
    // Harpoon slot data (persisted)
    harpoon_data: HarpoonData,

    // Host folder readiness (set after $HOME resolution + change_host_folder)
    host_folder_ready: bool,
    pending_actions: Vec<PendingAction>,
    error: Option<String>,

    // Current session info
    current_session_name: Option<String>,
    sessions: Option<Vec<SessionInfo>>,

    // Current pane state
    focused_pane: Option<Pane>,
    tab_info: Option<Vec<TabInfo>>,
    pane_manifest: Option<PaneManifest>,

    // UI state
    selected_index: usize,
}

impl State {
    fn select_down(&mut self) {
        let count = self.harpoon_data.slots.len();
        if count == 0 {
            return;
        }
        self.selected_index = (self.selected_index + 1) % count;
    }

    fn select_up(&mut self) {
        let count = self.harpoon_data.slots.len();
        if count == 0 {
            return;
        }
        if self.selected_index == 0 {
            self.selected_index = count - 1;
        } else {
            self.selected_index -= 1;
        }
    }

    fn selected_key(&self) -> Option<char> {
        self.harpoon_data
            .slots
            .keys()
            .nth(self.selected_index)
            .copied()
    }

    fn update_panes(&mut self) -> Option<()> {
        let pane_manifest = self.pane_manifest.as_ref()?;
        let tab_info = self.tab_info.as_ref()?;

        let focused_tab = get_focused_tab(tab_info)?;
        let focused_pane_info = get_focused_pane(focused_tab.position, pane_manifest)?;

        self.focused_pane = Some(Pane {
            pane_info: focused_pane_info,
            tab_info: focused_tab,
        });

        Some(())
    }

    fn sync_slots_with_state(&mut self) {
        let Some(tab_info) = &self.tab_info else {
            return;
        };
        let Some(session_name) = &self.current_session_name else {
            return;
        };

        let mut changed = false;

        for slot in self.harpoon_data.slots.values_mut() {
            if &slot.session_name != session_name {
                continue;
            }

            if let Some(tab) = tab_info.iter().find(|t| t.name == slot.tab_name) {
                if slot.tab_position != tab.position {
                    slot.tab_position = tab.position;
                    changed = true;
                }
            }

            if let Some(pane_manifest) = &self.pane_manifest {
                if let Some(panes) = pane_manifest.panes.get(&slot.tab_position) {
                    if let Some(pane) = panes.iter().find(|p| p.id == slot.pane_id) {
                        if slot.pane_title != pane.title {
                            slot.pane_title = pane.title.clone();
                            changed = true;
                        }
                    }
                }
            }
        }

        if changed {
            self.save_to_disk();
        }
    }

    fn assign_slot(&mut self, key: char) {
        let Some(session_name) = self.current_session_name.clone() else {
            return;
        };
        let Some(focused_pane) = self.focused_pane.clone() else {
            return;
        };
        self.load_from_disk();

        self.harpoon_data.slots.insert(
            key,
            HarpoonSlot {
                session_name,
                tab_name: focused_pane.tab_info.name.clone(),
                tab_position: focused_pane.tab_info.position,
                pane_id: focused_pane.pane_info.id,
                pane_title: focused_pane.pane_info.title.clone(),
            },
        );

        self.save_to_disk();
    }

    fn delete_slot(&mut self, key: char) {
        self.load_from_disk();
        self.harpoon_data.slots.remove(&key);
        self.save_to_disk();
    }

    fn delete_selected_slot(&mut self) {
        let Some(key) = self.selected_key() else {
            return;
        };
        self.delete_slot(key);
        let count = self.harpoon_data.slots.len();
        if count > 0 && self.selected_index >= count {
            self.selected_index = count - 1;
        }
    }

    fn jump_to_slot(&mut self, key: char) {
        self.load_from_disk();

        let Some(slot_data) = self.harpoon_data.slots.get(&key) else {
            return;
        };

        let current_session = self.current_session_name.as_ref();

        if current_session == Some(&slot_data.session_name) {
            focus_terminal_pane(slot_data.pane_id, true);
        } else {
            switch_session_with_focus(
                &slot_data.session_name,
                Some(slot_data.tab_position),
                Some((slot_data.pane_id, false)),
            );
        }
        hide_self();
    }

    fn is_session_online(&self, session_name: &str) -> bool {
        self.sessions
            .as_ref()
            .map(|sessions| sessions.iter().any(|s| s.name == session_name))
            .unwrap_or(false)
    }

    fn save_to_disk(&mut self) {
        let json = serde_json::to_string(&self.harpoon_data).unwrap_or_default();
        debug_log!(
            "save_to_disk: writing {} slots",
            self.harpoon_data.slots.len()
        );
        match std::fs::write("/host/harpoon.json", &json) {
            Ok(()) => {
                debug_log!("save_to_disk: success");
            }
            Err(e) => {
                debug_log!("save_to_disk: error: {e}");
                self.error = Some(format!("Save failed: {e}"));
            }
        }
    }

    fn load_from_disk(&mut self) {
        if !self.host_folder_ready {
            debug_log!("load_from_disk: skipped (host folder not ready)");
            return;
        }
        debug_log!("load_from_disk: reading /host/harpoon.json");
        match std::fs::read_to_string("/host/harpoon.json") {
            Ok(contents) => match serde_json::from_str::<HarpoonData>(contents.trim()) {
                Ok(data) => {
                    debug_log!("load_from_disk: loaded {} slots", data.slots.len());
                    self.harpoon_data = data;
                }
                Err(e) => {
                    debug_log!("load_from_disk: parse error: {e}");
                    self.error = Some(format!("Parse failed: {e}"));
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                debug_log!("load_from_disk: file not found, using default");
                self.harpoon_data = HarpoonData::default();
            }
            Err(e) => {
                debug_log!("load_from_disk: error: {e}");
                self.error = Some(format!("Load failed: {e}"));
            }
        }
    }

    fn handle_command_result(
        &mut self,
        exit_code: Option<i32>,
        stdout: Vec<u8>,
        context: BTreeMap<String, String>,
    ) {
        let action = context.get("action").map(|s| s.as_str());

        match action {
            Some("resolve_home") => {
                debug_log!("handle_command_result: resolve_home, exit_code={exit_code:?}");
                if exit_code != Some(0) {
                    self.error = Some("Failed to resolve $HOME".to_string());
                    return;
                }
                let Ok(home) = String::from_utf8(stdout) else {
                    self.error = Some("$HOME is not valid UTF-8".to_string());
                    return;
                };
                let home = home.trim();
                if home.is_empty() {
                    self.error = Some("$HOME is empty".to_string());
                    return;
                }

                let config_dir = format!("{home}/.config/zellij");
                run_command(
                    &["mkdir", "-p", &config_dir],
                    BTreeMap::from([
                        ("action".to_string(), "mkdir_config".to_string()),
                        ("config_dir".to_string(), config_dir.clone()),
                    ]),
                );
            }
            Some("mkdir_config") => {
                debug_log!("handle_command_result: mkdir_config, exit_code={exit_code:?}");
                if exit_code != Some(0) {
                    self.error = Some("Failed to create config directory".to_string());
                    return;
                }
                let Some(config_dir) = context.get("config_dir") else {
                    self.error = Some("Missing config_dir in context".to_string());
                    return;
                };

                change_host_folder(std::path::PathBuf::from(config_dir));
                debug_log!("handle_command_result: host folder set to {config_dir}");
                self.host_folder_ready = true;
                self.load_from_disk();
                debug_log!(
                    "handle_command_result: draining {} pending actions",
                    self.pending_actions.len()
                );
                self.drain_pending_actions();
            }
            _ => {}
        }
    }

    fn drain_pending_actions(&mut self) {
        let actions: Vec<PendingAction> = self.pending_actions.drain(..).collect();
        for action in actions {
            match action {
                PendingAction::Jump(slot) => self.jump_to_slot(slot),
                PendingAction::Assign(slot) => self.assign_slot(slot),
            }
        }
    }
}

register_plugin!(State);

impl ZellijPlugin for State {
    fn load(&mut self, _config: BTreeMap<String, String>) {
        request_permission(&[
            PermissionType::ReadApplicationState,
            PermissionType::ChangeApplicationState,
            PermissionType::RunCommands,
            PermissionType::FullHdAccess,
        ]);
        subscribe(&[
            EventType::Key,
            EventType::TabUpdate,
            EventType::PaneUpdate,
            EventType::SessionUpdate,
            EventType::ModeUpdate,
            EventType::RunCommandResult,
            EventType::PermissionRequestResult,
        ]);
    }

    fn update(&mut self, event: Event) -> bool {
        let mut should_render = false;

        match event {
            Event::PermissionRequestResult(PermissionStatus::Granted) => {
                run_command(
                    &["sh", "-c", "echo $HOME"],
                    BTreeMap::from([("action".to_string(), "resolve_home".to_string())]),
                );
                should_render = true;
            }
            Event::PermissionRequestResult(PermissionStatus::Denied) => {
                self.error =
                    Some("Permissions denied. Please grant all requested permissions.".to_string());
                should_render = true;
            }
            Event::RunCommandResult(exit_code, stdout, _stderr, context) => {
                self.handle_command_result(exit_code, stdout, context);
                should_render = true;
            }
            Event::ModeUpdate(mode_info) => {
                self.current_session_name = mode_info.session_name;
                should_render = true;
            }
            Event::SessionUpdate(sessions, _resurrectable_sessions) => {
                self.sessions = Some(sessions);
                should_render = true;
            }
            Event::TabUpdate(tab_info) => {
                self.tab_info = Some(tab_info);
                self.update_panes();
                self.sync_slots_with_state();
                should_render = true;
            }
            Event::PaneUpdate(pane_manifest) => {
                self.pane_manifest = Some(pane_manifest);
                self.update_panes();
                self.sync_slots_with_state();
                should_render = true;
            }
            Event::Key(key) => match key.bare_key {
                BareKey::Char(c)
                    if c.is_ascii_uppercase() && is_slot_key(c.to_ascii_lowercase()) =>
                {
                    self.assign_slot(c.to_ascii_lowercase());
                    should_render = true;
                }
                BareKey::Char(c) if is_slot_key(c) && key.has_no_modifiers() => {
                    self.jump_to_slot(c);
                }
                BareKey::Char('d') if key.has_modifiers(&[KeyModifier::Ctrl]) => {
                    self.delete_selected_slot();
                    should_render = true;
                }
                BareKey::Down | BareKey::Char('j') => {
                    self.select_down();
                    should_render = true;
                }
                BareKey::Up | BareKey::Char('k') => {
                    self.select_up();
                    should_render = true;
                }
                BareKey::Enter => {
                    if let Some(key) = self.selected_key() {
                        self.jump_to_slot(key);
                    }
                }
                BareKey::Esc => {
                    hide_self();
                }
                _ => (),
            },
            _ => (),
        };

        should_render
    }

    fn pipe(&mut self, pipe_message: PipeMessage) -> bool {
        debug_log!(
            "pipe: name={}, payload={:?}",
            pipe_message.name,
            pipe_message.payload
        );
        let slot_key = pipe_message
            .payload
            .as_deref()
            .and_then(|p| p.chars().next())
            .filter(|c| is_slot_key(*c));

        let Some(slot_key) = slot_key else {
            return false;
        };

        match pipe_message.name.as_str() {
            "jump" => {
                if self.host_folder_ready {
                    self.jump_to_slot(slot_key);
                } else {
                    self.pending_actions.push(PendingAction::Jump(slot_key));
                }
            }
            "assign" => {
                if self.host_folder_ready {
                    self.assign_slot(slot_key);
                } else {
                    self.pending_actions.push(PendingAction::Assign(slot_key));
                }
            }
            _ => {}
        }

        false
    }

    fn render(&mut self, _rows: usize, cols: usize) {
        self.load_from_disk();

        let title = "─ Harpoon ";
        let title_suffix = "─".repeat(cols.saturating_sub(title.len() + 2));
        println!("{}{}", title.bold(), title_suffix);
        println!();

        if let Some(err) = &self.error {
            println!("  {}", err.red());
            println!();
        } else if !self.host_folder_ready {
            println!("  {}", "Loading...".dimmed());
            println!();
        }

        if self.harpoon_data.slots.is_empty() {
            println!("  {}", "No slots assigned.".dimmed());
            println!(
                "  {}",
                "Press Shift+letter to assign current pane.".dimmed()
            );
        } else {
            for (i, (key, slot)) in self.harpoon_data.slots.iter().enumerate() {
                let online = self.is_session_online(&slot.session_name);
                let status = if online { "" } else { " (offline)" };
                let slot_display = format!(
                    "[{}] {} | {} | {}{}",
                    key, slot.session_name, slot.tab_name, slot.pane_title, status
                );

                if i == self.selected_index {
                    println!("  {}", slot_display.red().bold());
                } else {
                    println!("  {}", slot_display);
                }
            }
        }

        println!();

        println!(
            "  {} {}  {} {}  {} {}",
            "a-z:".dimmed(),
            "jump",
            "A-Z:".dimmed(),
            "assign",
            "Ctrl+d:".dimmed(),
            "delete"
        );
        println!(
            "  {} {}  {} {}",
            "j/k/↑/↓:".dimmed(),
            "navigate",
            "Esc:".dimmed(),
            "close"
        );

        if let Some(pane) = &self.focused_pane {
            println!();
            println!(
                "  {} {} | {}",
                "Current:".dimmed(),
                pane.tab_info.name,
                pane.pane_info.title
            );
        }
    }
}
