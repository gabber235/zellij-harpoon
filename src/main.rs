use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use owo_colors::OwoColorize;
use zellij_tile::prelude::*;

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
    pub slots: Vec<Option<HarpoonSlot>>,
}

impl HarpoonData {
    fn ensure_slot(&mut self, index: usize) {
        while self.slots.len() <= index {
            self.slots.push(None);
        }
    }

    fn get_slot(&self, index: usize) -> Option<&HarpoonSlot> {
        self.slots.get(index).and_then(|s| s.as_ref())
    }
}

// ----------------------------------- Legacy Pane Display -----------------------------------

#[derive(Clone, Serialize, Deserialize)]
pub struct Pane {
    pub pane_info: PaneInfo,
    pub tab_info: TabInfo,
}

// ----------------------------------- Pending Actions -----------------------------------

enum PendingAction {
    Jump(usize),
    Assign(usize),
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

// Slot key mapping: y=0, u=1, i=2, o=3, p=4
fn char_to_slot_index(c: char) -> Option<usize> {
    match c.to_ascii_lowercase() {
        'y' => Some(0),
        'u' => Some(1),
        'i' => Some(2),
        'o' => Some(3),
        'p' => Some(4),
        _ => None,
    }
}

fn slot_index_to_char(index: usize) -> char {
    match index {
        0 => 'y',
        1 => 'u',
        2 => 'i',
        3 => 'o',
        4 => 'p',
        _ => '?',
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
    selected_slot: usize,
    awaiting_delete_key: bool,
}

impl State {
    fn select_down(&mut self) {
        self.selected_slot = (self.selected_slot + 1) % 5;
    }

    fn select_up(&mut self) {
        if self.selected_slot == 0 {
            self.selected_slot = 4;
        } else {
            self.selected_slot -= 1;
        }
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

        for slot in self.harpoon_data.slots.iter_mut().flatten() {
            // Only update slots for the current session
            if &slot.session_name != session_name {
                continue;
            }

            // Update tab position if tab was moved/renamed
            if let Some(tab) = tab_info.iter().find(|t| t.name == slot.tab_name) {
                if slot.tab_position != tab.position {
                    slot.tab_position = tab.position;
                    changed = true;
                }
            }

            // Update pane title if changed
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

    fn assign_slot(&mut self, slot_index: usize) {
        let Some(session_name) = self.current_session_name.clone() else {
            return;
        };
        let Some(focused_pane) = self.focused_pane.clone() else {
            return;
        };
        self.load_from_disk();

        self.harpoon_data.ensure_slot(slot_index);
        self.harpoon_data.slots[slot_index] = Some(HarpoonSlot {
            session_name,
            tab_name: focused_pane.tab_info.name.clone(),
            tab_position: focused_pane.tab_info.position,
            pane_id: focused_pane.pane_info.id,
            pane_title: focused_pane.pane_info.title.clone(),
        });

        self.save_to_disk();
        hide_self();
    }

    fn delete_slot(&mut self, slot_index: usize) {
        // Re-read from disk to pick up changes from other session instances
        self.load_from_disk();

        if slot_index < self.harpoon_data.slots.len() {
            self.harpoon_data.slots[slot_index] = None;
            self.save_to_disk();
        }
    }

    fn jump_to_slot(&mut self, slot_index: usize) {
        // Re-read from disk to pick up changes from other session instances
        self.load_from_disk();

        let Some(slot_data) = self.harpoon_data.get_slot(slot_index) else {
            return;
        };

        let current_session = self.current_session_name.as_ref();

        if current_session == Some(&slot_data.session_name) {
            // Same session: just focus the pane directly (faster)
            focus_terminal_pane(slot_data.pane_id, true);
        } else {
            // Different session: switch session and focus pane
            switch_session_with_focus(
                &slot_data.session_name,
                Some(slot_data.tab_position),
                Some((slot_data.pane_id, false)), // (pane_id, is_plugin)
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
        match std::fs::write("/host/harpoon.json", &json) {
            Ok(()) => {}
            Err(e) => {
                self.error = Some(format!("Save failed: {e}"));
            }
        }
    }

    fn load_from_disk(&mut self) {
        if !self.host_folder_ready {
            return;
        }
        match std::fs::read_to_string("/host/harpoon.json") {
            Ok(contents) => match serde_json::from_str::<HarpoonData>(contents.trim()) {
                Ok(data) => self.harpoon_data = data,
                Err(e) => self.error = Some(format!("Parse failed: {e}")),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.harpoon_data = HarpoonData::default();
            }
            Err(e) => self.error = Some(format!("Load failed: {e}")),
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
                if exit_code != Some(0) {
                    self.error = Some("Failed to create config directory".to_string());
                    return;
                }
                let Some(config_dir) = context.get("config_dir") else {
                    self.error = Some("Missing config_dir in context".to_string());
                    return;
                };

                change_host_folder(std::path::PathBuf::from(config_dir));
                self.host_folder_ready = true;
                self.load_from_disk();
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
            Event::Key(key) => {
                // Handle delete mode (awaiting second key)
                if self.awaiting_delete_key {
                    self.awaiting_delete_key = false;
                    if let BareKey::Char(c) = key.bare_key {
                        if let Some(slot_index) = char_to_slot_index(c) {
                            self.delete_slot(slot_index);
                            should_render = true;
                            return should_render;
                        }
                    }
                    // Invalid key after 'd', just ignore and re-render
                    should_render = true;
                    return should_render;
                }

                match key.bare_key {
                    // Lowercase y/u/i/o/p: Jump to slot
                    BareKey::Char('y') => self.jump_to_slot(0),
                    BareKey::Char('u') => self.jump_to_slot(1),
                    BareKey::Char('i') => self.jump_to_slot(2),
                    BareKey::Char('o') => self.jump_to_slot(3),
                    BareKey::Char('p') => self.jump_to_slot(4),

                    // Uppercase Y/U/I/O/P: Assign (override) slot
                    BareKey::Char('Y') => self.assign_slot(0),
                    BareKey::Char('U') => self.assign_slot(1),
                    BareKey::Char('I') => self.assign_slot(2),
                    BareKey::Char('O') => self.assign_slot(3),
                    BareKey::Char('P') => self.assign_slot(4),

                    // Delete: press 'd' then slot key
                    BareKey::Char('d') => {
                        self.awaiting_delete_key = true;
                        should_render = true;
                    }

                    // Navigation
                    BareKey::Down | BareKey::Char('j') => {
                        self.select_down();
                        should_render = true;
                    }
                    BareKey::Up | BareKey::Char('k') => {
                        self.select_up();
                        should_render = true;
                    }

                    // Jump to selected slot with Enter/l
                    BareKey::Enter | BareKey::Char('l') => {
                        self.jump_to_slot(self.selected_slot);
                    }

                    // Reload from disk
                    BareKey::Char('r') => {
                        self.load_from_disk();
                        should_render = true;
                    }

                    // Close
                    BareKey::Char('c') | BareKey::Esc => {
                        hide_self();
                    }

                    _ => (),
                }
            }
            _ => (),
        };

        should_render
    }

    fn pipe(&mut self, pipe_message: PipeMessage) -> bool {
        let slot = pipe_message
            .payload
            .as_deref()
            .and_then(|p| p.parse::<usize>().ok());

        let Some(slot) = slot else {
            return false;
        };

        match pipe_message.name.as_str() {
            "jump" => {
                if self.host_folder_ready {
                    self.jump_to_slot(slot);
                } else {
                    self.pending_actions.push(PendingAction::Jump(slot));
                }
            }
            "assign" => {
                if self.host_folder_ready {
                    self.assign_slot(slot);
                } else {
                    self.pending_actions.push(PendingAction::Assign(slot));
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

        // Show loading or error state
        if let Some(err) = &self.error {
            println!("  {}", err.red());
            println!();
        } else if !self.host_folder_ready {
            println!("  {}", "Loading...".dimmed());
            println!();
        }

        // Render the 5 slots
        for i in 0..5 {
            let slot_char = slot_index_to_char(i);
            let is_selected = i == self.selected_slot;

            let slot_display = if let Some(slot) = self.harpoon_data.get_slot(i) {
                let online = self.is_session_online(&slot.session_name);
                let status = if online { "" } else { " (offline)" };
                format!(
                    "[{}] {} | {} | {}{}",
                    slot_char, slot.session_name, slot.tab_name, slot.pane_title, status
                )
            } else {
                format!("[{}] (empty)", slot_char)
            };

            if is_selected {
                println!("  {}", slot_display.red().bold());
            } else {
                println!("  {}", slot_display);
            }
        }

        println!();

        // Help text
        if self.awaiting_delete_key {
            println!("  {}", "Press y/u/i/o/p to delete that slot...".yellow());
        } else {
            println!(
                "  {} {}  {} {}",
                "y/u/i/o/p:".dimmed(),
                "jump",
                "Y/U/I/O/P:".dimmed(),
                "assign"
            );
            println!(
                "  {} {}  {} {}  {} {}",
                "d + key:".dimmed(),
                "delete",
                "r:".dimmed(),
                "reload",
                "Esc:".dimmed(),
                "close"
            );
        }

        // Show current pane info
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
