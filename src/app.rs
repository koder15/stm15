use crate::tunnel::{self, TunnelConfig, TunnelManager, TunnelStatus};
use ratatui::{
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, TableState},
    Frame,
};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// ── actions ─────────────────────────────────────────────────────────

pub enum Action {
    Start(usize),
    Stop(usize),
    Restart(usize),
    AddTunnel(TunnelConfig),
    EditTunnel(usize, TunnelConfig),
    DeleteTunnel(usize),
    StartAll,
    StopAll,
    HealthCheck,
    Quit,
}

// ── input field ─────────────────────────────────────────────────────

struct InputField {
    label: String,
    buffer: String,
    cursor: usize,
}

impl InputField {
    fn new(label: &str, value: &str) -> Self {
        InputField {
            label: label.to_string(),
            buffer: value.to_string(),
            cursor: value.len(),
        }
    }

    fn handle_key(&mut self, c: char) {
        self.buffer.insert(self.cursor, c);
        self.cursor += 1;
    }

    fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.buffer.remove(self.cursor);
        }
    }

    fn delete(&mut self) {
        if self.cursor < self.buffer.len() {
            self.buffer.remove(self.cursor);
        }
    }

    fn cursor_left(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
        }
    }

    fn cursor_right(&mut self) {
        if self.cursor < self.buffer.len() {
            self.cursor += 1;
        }
    }

    fn home(&mut self) {
        self.cursor = 0;
    }

    fn end(&mut self) {
        self.cursor = self.buffer.len();
    }

    fn render(&self, width: u16, active: bool) -> Line<'static> {
        let style = if active {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::REVERSED)
        } else {
            Style::default().fg(Color::White)
        };

        let label = format!("{:>14}: ", self.label);
        let visible_width = width.saturating_sub(label.len() as u16 + 2);
        if visible_width == 0 {
            return Line::from(vec![
                Span::styled(label, style),
                Span::styled("…", Style::default().fg(Color::DarkGray)),
            ]);
        }

        let start = if self.cursor as u16 >= visible_width {
            self.cursor.saturating_sub(visible_width as usize - 1)
        } else {
            0
        };
        let end = (start + visible_width as usize).min(self.buffer.len());
        let display = &self.buffer[start..end];
        let rel_cursor = self.cursor.saturating_sub(start);

        let mut spans: Vec<Span> = vec![Span::styled(label, style)];

        if display.is_empty() {
            spans.push(Span::styled(" ", style));
        } else {
            for (i, ch) in display.chars().enumerate() {
                let cs = if active && i == rel_cursor {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)
                } else {
                    style
                };
                spans.push(Span::styled(ch.to_string(), cs));
            }
        }

        // If cursor is at end of buffer beyond visible range
        if active && self.cursor >= start + visible_width as usize {
            spans.push(Span::styled(
                " ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ));
        }

        Line::from(spans)
    }
}

// ── form screen ──────────────────────────────────────────────────────

#[derive(PartialEq, Clone)]
enum FieldType {
    Text,
    Select { options: Vec<String>, index: usize },
}

struct FormField {
    label: String,
    input: InputField,
    field_type: FieldType,
}

struct FormScreen {
    title: String,
    fields: Vec<FormField>,
    active: usize,
}

impl FormScreen {
    fn new_add() -> Self {
        Self::from_config(
            "Add Tunnel",
            &TunnelConfig {
                name: String::new(),
                tunnel_type: "local".into(),
                local_port: 0,
                ssh_host: String::new(),
                ssh_port: 22,
                ssh_user: String::new(),
                ssh_key: String::new(),
                remote_host: "localhost".into(),
                remote_port: 0,
                enabled: true,
            },
        )
    }

    fn new_edit(config: &TunnelConfig) -> Self {
        Self::from_config("Edit Tunnel", config)
    }

    fn from_config(title: &str, c: &TunnelConfig) -> Self {
        let build_select = |opts: Vec<String>, current: &str| (opts.clone(), {
            let idx = opts.iter().position(|o| o == current).unwrap_or(0);
            (idx, InputField::new("", &opts[idx]))
        });

        let (type_opts, (type_idx, type_input)) =
            build_select(vec!["local".into(), "remote".into(), "dynamic".into()], &c.tunnel_type);

        let (en_opts, (en_idx, en_input)) =
            build_select(vec!["true".into(), "false".into()], if c.enabled { "true" } else { "false" });

        let mut fields = Vec::new();

        fields.push(FormField {
            label: "Name".into(),
            input: InputField::new("Name", &c.name),
            field_type: FieldType::Text,
        });

        fields.push(FormField {
            label: "Type".into(),
            input: type_input,
            field_type: FieldType::Select {
                options: type_opts,
                index: type_idx,
            },
        });

        fields.push(FormField {
            label: "Local Port".into(),
            input: InputField::new("Local Port", &if c.local_port > 0 { c.local_port.to_string() } else { String::new() }),
            field_type: FieldType::Text,
        });

        fields.push(FormField {
            label: "SSH Host".into(),
            input: InputField::new("SSH Host", &c.ssh_host),
            field_type: FieldType::Text,
        });

        fields.push(FormField {
            label: "SSH Port".into(),
            input: InputField::new("SSH Port", &c.ssh_port.to_string()),
            field_type: FieldType::Text,
        });

        fields.push(FormField {
            label: "SSH User".into(),
            input: InputField::new("SSH User", &c.ssh_user),
            field_type: FieldType::Text,
        });

        fields.push(FormField {
            label: "SSH Key".into(),
            input: InputField::new("SSH Key", &c.ssh_key),
            field_type: FieldType::Text,
        });

        fields.push(FormField {
            label: "Remote Host".into(),
            input: InputField::new("Remote Host", &c.remote_host),
            field_type: FieldType::Text,
        });

        fields.push(FormField {
            label: "Remote Port".into(),
            input: InputField::new("Remote Port", &if c.remote_port > 0 { c.remote_port.to_string() } else { String::new() }),
            field_type: FieldType::Text,
        });

        fields.push(FormField {
            label: "Auto-start".into(),
            input: en_input,
            field_type: FieldType::Select {
                options: en_opts,
                index: en_idx,
            },
        });

        FormScreen {
            title: title.to_string(),
            fields,
            active: 0,
        }
    }

    fn handle_key(&mut self, c: char) {
        let field = &mut self.fields[self.active];
        field.input.handle_key(c);
        if let FieldType::Select { options, index } = &mut field.field_type {
            *index = options
                .iter()
                .position(|o| o == &field.input.buffer)
                .unwrap_or(*index);
        }
    }

    fn cycle_select(&mut self, forward: bool) {
        let field = &mut self.fields[self.active];
        if let FieldType::Select { options, index } = &mut field.field_type {
            if forward {
                *index = (*index + 1) % options.len();
            } else {
                *index = (*index + options.len() - 1) % options.len();
            }
            field.input.buffer = options[*index].clone();
            field.input.cursor = field.input.buffer.len();
        }
    }

    fn next_field(&mut self) {
        self.active = (self.active + 1) % self.fields.len();
    }

    fn prev_field(&mut self) {
        self.active = (self.active + self.fields.len() - 1) % self.fields.len();
    }

    fn to_config(&self) -> Option<TunnelConfig> {
        let get = |label: &str| -> String {
            self.fields
                .iter()
                .find(|f| f.label == label)
                .map(|f| f.input.buffer.clone())
                .unwrap_or_default()
        };

        let name = get("Name");
        if name.is_empty() {
            return None;
        }

        Some(TunnelConfig {
            name,
            tunnel_type: get("Type"),
            local_port: get("Local Port").parse().unwrap_or(0),
            ssh_host: get("SSH Host"),
            ssh_port: get("SSH Port").parse().unwrap_or(22),
            ssh_user: get("SSH User"),
            ssh_key: get("SSH Key"),
            remote_host: get("Remote Host"),
            remote_port: get("Remote Port").parse().unwrap_or(0),
            enabled: get("Auto-start") == "true",
        })
    }

    fn render(&self, area: Rect, f: &mut Frame) {
        let width = area.width.saturating_sub(4);
        let content_height = self.fields.len() as u16 + 2;
        let popup_height = content_height + 3;
        let popup_width = width.min(56);

        let popup_area = centered_rect(area, popup_width, popup_height);
        f.render_widget(Clear, popup_area);

        let block = Block::default()
            .borders(Borders::ALL)
            .title(self.title.clone())
            .title_alignment(Alignment::Center)
            .style(Style::default().fg(Color::Cyan));
        let inner = block.inner(popup_area);

        let rows: Vec<Line> = self
            .fields
            .iter()
            .enumerate()
            .map(|(i, field)| field.input.render(inner.width, i == self.active))
            .collect();

        f.render_widget(Paragraph::new(ratatui::text::Text::from(rows)), inner);
        f.render_widget(block, popup_area);

        let help = Span::styled(
            "Tab/Up/Dn: next  |  Space: toggle  |  Enter: save  |  Esc: cancel",
            Style::default().fg(Color::DarkGray),
        );
        f.render_widget(
            Paragraph::new(Line::from(help)).alignment(Alignment::Center),
            Rect::new(
                popup_area.x + 1,
                popup_area.y + popup_height - 1,
                popup_area.width.saturating_sub(2),
                1,
            ),
        );
    }
}

// ── picker (host selector) ──────────────────────────────────────────

struct PickerScreen {
    items: Vec<(String, crate::tunnel::SshHost)>,
    selected: usize,
}

impl PickerScreen {
    fn from_ssh_config() -> Self {
        let hosts = crate::tunnel::parse_ssh_config();
        let items: Vec<_> = hosts.into_iter().map(|h| {
            let desc = if let Some(ref hostname) = h.hostname {
                if let Some(ref user) = h.user {
                    format!("{}  →  {}@{}:{}", h.name, user, hostname, h.port)
                } else {
                    format!("{}  →  {}:{}", h.name, hostname, h.port)
                }
            } else {
                h.name.clone()
            };
            (desc, h)
        }).collect();
        PickerScreen { items, selected: 0 }
    }

    fn selected_host(&self) -> Option<&crate::tunnel::SshHost> {
        self.items.get(self.selected).map(|(_, h)| h)
    }

    fn up(&mut self) {
        if !self.items.is_empty() {
            self.selected = (self.selected + self.items.len() - 1) % self.items.len();
        }
    }

    fn down(&mut self) {
        if !self.items.is_empty() {
            self.selected = (self.selected + 1) % self.items.len();
        }
    }

    fn render(&self, area: Rect, f: &mut Frame) {
        let h = (self.items.len() as u16).min(area.height.saturating_sub(4)) + 4;
        let popup = centered_rect(area, 60.min(area.width), h);
        f.render_widget(Clear, popup);

        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Import from SSH config ")
            .title_alignment(Alignment::Center)
            .style(Style::default().fg(Color::Cyan));
        let inner = block.inner(popup);

        let lines: Vec<Line> = self
            .items
            .iter()
            .enumerate()
            .map(|(i, (desc, _))| {
                let style = if i == self.selected {
                    Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                };
                Line::from(Span::styled(format!(" {} ", desc), style))
            })
            .collect();

        if lines.is_empty() {
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    " No hosts found in ~/.ssh/config ",
                    Style::default().fg(Color::DarkGray),
                ))),
                inner,
            );
        } else {
            f.render_widget(Paragraph::new(ratatui::text::Text::from(lines)), inner);
        }
        f.render_widget(block, popup);

        let help = Span::styled(
            " j/k: navigate  |  Enter: select  |  Esc: cancel ",
            Style::default().fg(Color::DarkGray),
        );
        f.render_widget(
            Paragraph::new(Line::from(help)).alignment(Alignment::Center),
            Rect::new(popup.x + 1, popup.y + popup.height - 1, popup.width.saturating_sub(2), 1),
        );
    }
}

// ── confirm screen ──────────────────────────────────────────────────

pub struct ConfirmScreen {
    message: String,
}

impl ConfirmScreen {
    pub fn new(message: &str) -> Self {
        ConfirmScreen {
            message: message.to_string(),
        }
    }

    fn render(&self, area: Rect, f: &mut Frame) {
        let popup_area = centered_rect(area, 42, 6);
        f.render_widget(Clear, popup_area);

        let block = Block::default()
            .borders(Borders::ALL)
            .style(Style::default().fg(Color::Yellow));
        let inner = block.inner(popup_area);

        let msg = Paragraph::new(self.message.clone())
            .alignment(Alignment::Center)
            .style(Style::default().fg(Color::White));
        f.render_widget(msg, Rect::new(inner.x, inner.y, inner.width, 2));

        let help = Paragraph::new(Line::from(vec![
            Span::styled(
                " [Y]es ",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(
                " [N]o ",
                Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
            ),
        ]))
        .alignment(Alignment::Center);
        f.render_widget(help, Rect::new(inner.x, inner.y + 2, inner.width, 1));

        f.render_widget(block, popup_area);
    }
}

// ── app ─────────────────────────────────────────────────────────────

enum AppMode {
    Normal,
    Form(FormScreen),
    Confirm(ConfirmScreen),
    Picker(PickerScreen),
}

pub struct App {
    pub manager: Arc<Mutex<TunnelManager>>,
    table_state: TableState,
    mode: AppMode,
    notification: Option<(String, Instant)>,
    pending_actions: Vec<Action>,
    editing_index: Option<usize>,
}

impl App {
    pub fn new(manager: Arc<Mutex<TunnelManager>>) -> Self {
        let mut table_state = TableState::default();
        table_state.select(Some(0));

        App {
            manager,
            table_state,
            mode: AppMode::Normal,
            notification: None,
            pending_actions: Vec::new(),
            editing_index: None,
        }
    }

    pub fn selected(&self) -> Option<usize> {
        let i = self.table_state.selected().unwrap_or(usize::MAX);
        if i < self.tunnel_count() {
            Some(i)
        } else {
            None
        }
    }

    fn tunnel_count(&self) -> usize {
        self.manager.lock().map(|m| m.tunnels.len()).unwrap_or(0)
    }

    fn notify(&mut self, msg: &str) {
        self.notification = Some((msg.to_string(), Instant::now()));
    }

    pub fn take_actions(&mut self) -> Vec<Action> {
        let mut actions = Vec::new();
        std::mem::swap(&mut actions, &mut self.pending_actions);
        actions
    }

    fn clear_notification(&mut self) {
        if let Some((_, ts)) = &self.notification {
            if ts.elapsed() > Duration::from_secs(5) {
                self.notification = None;
            }
        }
    }

    // ── key handling ────────────────────────────────────────────────

    pub fn handle_key(&mut self, key: crossterm::event::KeyEvent) {
        // Esc always cancels modals
        if key.code == crossterm::event::KeyCode::Esc {
            if matches!(&self.mode, AppMode::Form(_) | AppMode::Confirm(_) | AppMode::Picker(_)) {
                self.mode = AppMode::Normal;
                return;
            }
        }

        let old_mode = std::mem::replace(&mut self.mode, AppMode::Normal);
        let new_mode = match old_mode {
            AppMode::Form(mut form) => {
                self.handle_form(key, &mut form);
                AppMode::Form(form)
            }
            AppMode::Confirm(confirm) => {
                self.handle_confirm(key);
                AppMode::Confirm(confirm)
            }
            AppMode::Picker(mut picker) => {
                self.handle_picker(key, &mut picker);
                AppMode::Picker(picker)
            }
            AppMode::Normal => {
                self.handle_normal(key);
                AppMode::Normal
            }
        };
        self.mode = new_mode;
    }

    fn handle_normal(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::{KeyCode, KeyModifiers};
        let n = self.tunnel_count();

        match key.code {
            KeyCode::Char('q') => {
                self.pending_actions.push(Action::Quit);
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if n > 0 {
                    let i = self.table_state.selected().unwrap_or(0);
                    let next = if i + 1 < n { i + 1 } else { 0 };
                    self.table_state.select(Some(next));
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if n > 0 {
                    let i = self.table_state.selected().unwrap_or(0);
                    let prev = if i > 0 { i - 1 } else { n - 1 };
                    self.table_state.select(Some(prev));
                }
            }
            KeyCode::Char('s') => {
                if let Some(i) = self.selected() {
                    self.pending_actions.push(Action::Start(i));
                    self.notify("Starting tunnel...");
                }
            }
            KeyCode::Char('K') => {
                if let Some(i) = self.selected() {
                    self.pending_actions.push(Action::Stop(i));
                    self.notify("Stopping tunnel...");
                }
            }
            KeyCode::Char('r') => {
                if let Some(i) = self.selected() {
                    self.pending_actions.push(Action::Restart(i));
                    self.notify("Restarting tunnel...");
                }
            }
            KeyCode::Char('a') => {
                self.mode = AppMode::Form(FormScreen::new_add());
            }
            KeyCode::Char('i') => {
                let picker = PickerScreen::from_ssh_config();
                if picker.items.is_empty() {
                    self.notify("No hosts in ~/.ssh/config");
                } else {
                    self.mode = AppMode::Picker(picker);
                }
            }
            KeyCode::Char('e') => {
                if let Some(i) = self.selected() {
                    if let Ok(mgr) = self.manager.lock() {
                        if let Some(t) = mgr.tunnels.get(i) {
                            self.editing_index = Some(i);
                            let form = FormScreen::new_edit(&t.config);
                            self.mode = AppMode::Form(form);
                        }
                    }
                }
            }
            KeyCode::Char('d') => {
                if let Some(i) = self.selected() {
                    if let Ok(mgr) = self.manager.lock() {
                        if let Some(t) = mgr.tunnels.get(i) {
                            self.mode = AppMode::Confirm(ConfirmScreen::new(&format!(
                                "Delete tunnel '{}'?",
                                t.config.name
                            )));
                        }
                    }
                }
            }
            KeyCode::Char('h') => {
                self.pending_actions.push(Action::HealthCheck);
                self.notify("Running health check...");
            }
            KeyCode::Char('A') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.pending_actions.push(Action::StartAll);
                self.notify("Starting all enabled tunnels...");
            }
            KeyCode::Char('X') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.pending_actions.push(Action::StopAll);
                self.notify("Stopping all tunnels...");
            }
            _ => {}
        }
    }

    fn handle_form(&mut self, key: crossterm::event::KeyEvent, form: &mut FormScreen) {
        use crossterm::event::KeyCode;

        match key.code {
            KeyCode::Enter => {
                if let Some(config) = form.to_config() {
                    if let Some(i) = self.editing_index.take() {
                        self.pending_actions.push(Action::EditTunnel(i, config));
                    } else {
                        self.pending_actions.push(Action::AddTunnel(config));
                    }
                    self.mode = AppMode::Normal;
                } else {
                    self.notify("Name is required");
                }
            }
            KeyCode::Tab | KeyCode::Down => {
                form.next_field();
            }
            KeyCode::BackTab | KeyCode::Up => {
                form.prev_field();
            }
            KeyCode::Char(' ') => {
                form.cycle_select(true);
            }
            KeyCode::Backspace => {
                form.fields[form.active].input.backspace();
            }
            KeyCode::Delete => {
                form.fields[form.active].input.delete();
            }
            KeyCode::Left => {
                form.fields[form.active].input.cursor_left();
            }
            KeyCode::Right => {
                form.fields[form.active].input.cursor_right();
            }
            KeyCode::Home => {
                form.fields[form.active].input.home();
            }
            KeyCode::End => {
                form.fields[form.active].input.end();
            }
            KeyCode::Char(c) => {
                form.handle_key(c);
            }
            _ => {}
        }
    }

    fn handle_confirm(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::KeyCode;

        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                if let Some(i) = self.selected() {
                    self.pending_actions.push(Action::DeleteTunnel(i));
                }
                self.mode = AppMode::Normal;
            }
            KeyCode::Char('n') | KeyCode::Char('N') => {
                self.mode = AppMode::Normal;
            }
            _ => {}
        }
    }

    fn handle_picker(&mut self, key: crossterm::event::KeyEvent, picker: &mut PickerScreen) {
        use crossterm::event::KeyCode;

        match key.code {
            KeyCode::Enter => {
                if let Some(host) = picker.selected_host() {
                    let config = TunnelConfig {
                        name: host.name.clone(),
                        tunnel_type: "local".into(),
                        local_port: 0,
                        ssh_host: host.hostname.clone().unwrap_or_default(),
                        ssh_port: host.port,
                        ssh_user: host.user.clone().unwrap_or_default(),
                        ssh_key: host.identity_file.clone().unwrap_or_default(),
                        remote_host: "localhost".into(),
                        remote_port: 0,
                        enabled: true,
                    };
                    self.mode = AppMode::Form(FormScreen::new_edit(&config));
                }
            }
            KeyCode::Char('j') | KeyCode::Down => picker.down(),
            KeyCode::Char('k') | KeyCode::Up => picker.up(),
            _ => {}
        }
    }

    // ── rendering ───────────────────────────────────────────────────

    pub fn render(&mut self, f: &mut Frame) {
        self.clear_notification();
        let area = f.area();

        let layout = Layout::default()
            .direction(ratatui::layout::Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(3),
                Constraint::Length(1),
            ])
            .split(area);

        self.render_header(f, layout[0]);
        self.render_table(f, layout[1]);
        self.render_status(f, layout[2]);

        match &self.mode {
            AppMode::Form(form) => form.render(area, f),
            AppMode::Confirm(confirm) => confirm.render(area, f),
            AppMode::Picker(picker) => picker.render(area, f),
            _ => {}
        }
    }

    fn render_header(&self, f: &mut Frame, area: Rect) {
        let text = Line::from(vec![
            Span::styled(
                " STM15 ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("─ SSH Tunnel Manager ", Style::default().fg(Color::DarkGray)),
        ]);
        f.render_widget(Paragraph::new(text), area);
    }

    fn render_table(&mut self, f: &mut Frame, area: Rect) {
        let snapshots: Vec<(TunnelConfig, TunnelStatus, Option<Instant>, String)> = {
            let mgr = self.manager.lock().unwrap();
            (0..mgr.tunnels.len())
                .map(|i| {
                    let config = mgr.tunnels[i].config.clone();
                    let (status, start, err) = mgr.snapshot(i).unwrap_or((
                        TunnelStatus::Stopped,
                        None,
                        String::new(),
                    ));
                    (config, status, start, err)
                })
                .collect()
        };

        let header = Row::new(vec![
            Cell::from(""),
            Cell::from(Span::styled(
                " Name ",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Cell::from(Span::styled(
                " Type ",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Cell::from(Span::styled(
                " Local ",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Cell::from(Span::styled(
                " Remote ",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Cell::from(Span::styled(
                " SSH Host ",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Cell::from(Span::styled(
                " Uptime ",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Cell::from(Span::styled(
                " Error ",
                Style::default().add_modifier(Modifier::BOLD),
            )),
        ]);

        let widths = [
            Constraint::Length(1),
            Constraint::Percentage(20),
            Constraint::Length(9),
            Constraint::Length(7),
            Constraint::Percentage(18),
            Constraint::Percentage(22),
            Constraint::Length(8),
            Constraint::Percentage(24),
        ];

        let rows: Vec<Row> = snapshots
            .iter()
            .map(|(config, status, start, err)| {
                let symbol = status.symbol();
                let color = match status {
                    TunnelStatus::Running => Color::Green,
                    TunnelStatus::Reconnecting { .. } => Color::Yellow,
                    TunnelStatus::Stopped => Color::DarkGray,
                };

                let uptime_str = match start {
                    Some(t) => {
                        let elapsed = t.elapsed().as_secs();
                        if elapsed >= 3600 {
                            format!("{}h{:02}m", elapsed / 3600, (elapsed % 3600) / 60)
                        } else if elapsed >= 60 {
                            format!("{}m{:02}s", elapsed / 60, elapsed % 60)
                        } else {
                            format!("{}s", elapsed)
                        }
                    }
                    None => "-".into(),
                };

                let remote = if config.remote_port > 0 {
                    format!("{}:{}", config.remote_host, config.remote_port)
                } else {
                    "-".into()
                };

                let ssh = if config.ssh_user.is_empty() {
                    config.ssh_host.clone()
                } else {
                    format!("{}@{}", config.ssh_user, config.ssh_host)
                };

                let err_display: String = err.chars().take(40).collect();

                Row::new(vec![
                    Cell::from(Span::styled(symbol, Style::default().fg(color))),
                    Cell::from(config.name.clone()),
                    Cell::from(Span::styled(
                        config.tunnel_type.clone(),
                        Style::default().fg(Color::Cyan),
                    )),
                    Cell::from(config.local_port.to_string()),
                    Cell::from(remote),
                    Cell::from(ssh),
                    Cell::from(uptime_str),
                    Cell::from(Span::styled(err_display, Style::default().fg(Color::Red))),
                ])
            })
            .collect();

        let table = Table::new(rows, widths)
            .header(header)
            .block(Block::default().borders(Borders::ALL))
            .highlight_style(
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("");

        f.render_stateful_widget(table, area, &mut self.table_state);
    }

    fn render_status(&self, f: &mut Frame, area: Rect) {
        let summary = self.manager.lock().map(|mgr| mgr.status_summary()).unwrap_or_else(|_| "…".into());

        let mut spans = vec![Span::styled(
            format!(" {} ", summary),
            Style::default().fg(Color::DarkGray),
        )];

        if let Some((msg, _)) = &self.notification {
            spans.push(Span::styled(
                format!(" | {} ", msg),
                Style::default().fg(Color::Yellow),
            ));
        }

        f.render_widget(Paragraph::new(Line::from(spans)), area);
    }
}

// ── action execution ────────────────────────────────────────────────

pub async fn execute_action(action: Action, manager: Arc<Mutex<TunnelManager>>, app: &mut App) {
    match action {
        Action::Start(i) => {
            let mgr = manager.lock().unwrap();
            let name = mgr.tunnels.get(i).map(|t| t.config.name.clone()).unwrap_or_default();
            mgr.start(i);
            drop(mgr);
            app.notify(&format!("Started: {name}"));
        }
        Action::Stop(i) => {
            let mgr = manager.lock().unwrap();
            let name = mgr.tunnels.get(i).map(|t| t.config.name.clone()).unwrap_or_default();
            mgr.stop(i);
            drop(mgr);
            app.notify(&format!("Stopped: {name}"));
        }
        Action::Restart(i) => {
            let mgr = manager.lock().unwrap();
            let name = mgr.tunnels.get(i).map(|t| t.config.name.clone()).unwrap_or_default();
            mgr.restart(i);
            drop(mgr);
            app.notify(&format!("Restarted: {name}"));
        }
        Action::AddTunnel(config) => {
            let name = config.name.clone();
            let enabled = config.enabled;
            {
                let mut mgr = manager.lock().unwrap();
                mgr.add_tunnel(config);
                let idx = mgr.tunnels.len() - 1;
                if enabled {
                    mgr.start(idx);
                }
                drop(mgr);
            }
            app.notify(&format!("Added: {name}"));
        }
        Action::EditTunnel(i, config) => {
            let name = config.name.clone();
            let enabled = config.enabled;
            {
                let mgr = manager.lock().unwrap();
                mgr.stop(i);
                drop(mgr);
            }
            {
                let mut mgr = manager.lock().unwrap();
                if let Some(t) = mgr.tunnels.get_mut(i) {
                    t.config = config;
                }
                mgr.save_config();
                if enabled {
                    mgr.start(i);
                }
                drop(mgr);
            }
            app.notify(&format!("Updated: {name}"));
        }
        Action::DeleteTunnel(i) => {
            let mut mgr = manager.lock().unwrap();
            let t = mgr.remove_tunnel(i);
            mgr.save_config();
            drop(mgr);
            if let Some(t) = t {
                app.notify(&format!("Deleted: {}", t.config.name));
            }
        }
        Action::StartAll => {
            let mgr = manager.lock().unwrap();
            mgr.start_all_enabled();
            drop(mgr);
            app.notify("Started all enabled tunnels");
        }
        Action::StopAll => {
            let mgr = manager.lock().unwrap();
            mgr.stop_all();
            drop(mgr);
            app.notify("Stopped all tunnels");
        }
        Action::HealthCheck => {
            let results = tunnel::force_health_check(manager.clone()).await;
            let msg: Vec<String> = results
                .iter()
                .map(|(name, ok)| format!("{name}: {}", if *ok { "OK" } else { "FAIL" }))
                .collect();
            if msg.is_empty() {
                app.notify("No running tunnels");
            } else {
                app.notify(&msg.join(" | "));
            }
        }
        Action::Quit => {
            app.notify("Quitting...");
        }
    }
}

// ── helpers ─────────────────────────────────────────────────────────

fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect::new(x, y, width.min(area.width), height.min(area.height))
}
