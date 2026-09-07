//! Interactive first-run onboarding wizard for Kobold.
//!
//! Guides the user through:
//! 1. Selecting detected coding agents (Claude Code, Grok, Antigravity, Codex, OpenCode).
//! 2. Choosing model providers (OpenAI, OpenRouter, Anthropic [disabled], Google [disabled]).
//! 3. On-demand adapter downloading and installation into `~/.local/bin/`.
//! 4. OAuth PKCE / API key credential setup.
//! 5. Parallel agent doctor verification and saving to `.kobold/settings.json`.

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use futures_util::StreamExt;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::app::Painted;
use crate::auth::{openrouter_oauth_flow, prompt_openai_key};
use crate::doctor::{check_all_agents, print_doctor_report, AgentSpec};
use crate::installer::install_required_adapters;
use crate::settings::{AgentSetting, ProviderSetting, Settings};
use crate::term::Screen;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WizardScreen {
    Agents,
    Providers,
    Done,
}

pub struct DetectedAgent {
    pub spec: AgentSpec,
    pub path: Option<PathBuf>,
    pub version: Option<String>,
}

pub struct WizardState {
    pub screen: WizardScreen,
    pub detected_agents: Vec<DetectedAgent>,
    pub agents_checked: Vec<bool>,
    pub selected_agent_row: usize, // 0 = Select All, 1..=5 = agents, 6 = Next

    pub selected_provider_row: usize, // 0 = OpenAI, 1 = OpenRouter, 2 = Anthropic, 3 = Google, 4 = Buttons
    pub provider_checked: [bool; 2],  // [OpenAI, OpenRouter]
    pub provider_button_focus: usize, // 0 = Back, 1 = Next
}

impl WizardState {
    pub fn new(detected: Vec<(AgentSpec, Option<PathBuf>, Option<String>)>) -> Self {
        let mut detected_agents = Vec::new();
        let mut agents_checked = Vec::new();

        for (spec, path, version) in detected {
            let is_detected = path.is_some();
            detected_agents.push(DetectedAgent {
                spec,
                path,
                version,
            });
            // Pre-check if detected
            agents_checked.push(is_detected);
        }

        Self {
            screen: WizardScreen::Agents,
            detected_agents,
            agents_checked,
            selected_agent_row: 0,
            selected_provider_row: 0,
            provider_checked: [false, false],
            provider_button_focus: 1, // Next button focused by default
        }
    }

    pub fn all_agents_selected(&self) -> bool {
        !self.agents_checked.is_empty() && self.agents_checked.iter().all(|&c| c)
    }

    pub fn toggle_select_all(&mut self) {
        let new_state = !self.all_agents_selected();
        for c in &mut self.agents_checked {
            *c = new_state;
        }
    }

    pub fn handle_key(&mut self, code: KeyCode, ctrl: bool) -> bool {
        if ctrl && code == KeyCode::Char('c') {
            return false; // Exit requested
        }

        match self.screen {
            WizardScreen::Agents => match code {
                KeyCode::Up => {
                    if self.selected_agent_row > 0 {
                        self.selected_agent_row -= 1;
                    }
                }
                KeyCode::Down => {
                    let max_row = self.detected_agents.len() + 1; // +1 for Select All, +1 for Next button
                    if self.selected_agent_row < max_row {
                        self.selected_agent_row += 1;
                    }
                }
                KeyCode::Char(' ') => {
                    if self.selected_agent_row == 0 {
                        self.toggle_select_all();
                    } else if self.selected_agent_row <= self.detected_agents.len() {
                        let idx = self.selected_agent_row - 1;
                        self.agents_checked[idx] = !self.agents_checked[idx];
                    }
                }
                KeyCode::Enter | KeyCode::Char('n') => {
                    let next_btn_row = self.detected_agents.len() + 1;
                    if self.selected_agent_row == next_btn_row || code == KeyCode::Char('n') {
                        self.screen = WizardScreen::Providers;
                        self.selected_provider_row = 0;
                    } else if self.selected_agent_row == 0 {
                        self.toggle_select_all();
                    } else if self.selected_agent_row <= self.detected_agents.len() {
                        let idx = self.selected_agent_row - 1;
                        self.agents_checked[idx] = !self.agents_checked[idx];
                    }
                }
                KeyCode::Tab => {
                    let max_row = self.detected_agents.len() + 1;
                    self.selected_agent_row = (self.selected_agent_row + 1) % (max_row + 1);
                }
                _ => {}
            },
            WizardScreen::Providers => match code {
                KeyCode::Up => {
                    if self.selected_provider_row > 0 {
                        self.selected_provider_row -= 1;
                    }
                }
                KeyCode::Down => {
                    if self.selected_provider_row < 4 {
                        self.selected_provider_row += 1;
                    }
                }
                KeyCode::Left => {
                    if self.selected_provider_row == 4 {
                        self.provider_button_focus = 0; // Back
                    }
                }
                KeyCode::Right => {
                    if self.selected_provider_row == 4 {
                        self.provider_button_focus = 1; // Next
                    }
                }
                KeyCode::Char(' ') => {
                    if self.selected_provider_row == 0 {
                        self.provider_checked[0] = !self.provider_checked[0];
                    } else if self.selected_provider_row == 1 {
                        self.provider_checked[1] = !self.provider_checked[1];
                    }
                }
                KeyCode::Enter => {
                    if self.selected_provider_row == 0 {
                        self.provider_checked[0] = !self.provider_checked[0];
                    } else if self.selected_provider_row == 1 {
                        self.provider_checked[1] = !self.provider_checked[1];
                    } else if self.selected_provider_row == 4 {
                        if self.provider_button_focus == 0 {
                            self.screen = WizardScreen::Agents;
                            self.selected_agent_row = self.detected_agents.len() + 1;
                        } else {
                            self.screen = WizardScreen::Done;
                        }
                    }
                }
                KeyCode::Tab => {
                    if self.selected_provider_row == 4 {
                        self.provider_button_focus = (self.provider_button_focus + 1) % 2;
                    } else {
                        self.selected_provider_row = (self.selected_provider_row + 1) % 5;
                    }
                }
                KeyCode::Char('b') => {
                    self.screen = WizardScreen::Agents;
                }
                KeyCode::Char('n') => {
                    self.screen = WizardScreen::Done;
                }
                _ => {}
            },
            WizardScreen::Done => {}
        }
        true
    }

    pub fn render(&self, buf: &mut Buffer, area: Rect) -> Painted {
        let header_style = Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD);
        let title_style = Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD);
        let muted_style = Style::default().fg(Color::DarkGray);
        let normal_style = Style::default().fg(Color::White);
        let cursor_style = Style::default()
            .bg(Color::Indexed(238))
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD);
        let btn_active_style = Style::default()
            .bg(Color::Cyan)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD);
        let btn_inactive_style = Style::default().bg(Color::Indexed(236)).fg(Color::White);

        put_str(
            buf,
            area.x + 2,
            area.y + 1,
            "KOBOLD FIRST-RUN SETUP WIZARD",
            header_style,
            area.width - 4,
        );
        put_str(
            buf,
            area.x + 2,
            area.y + 2,
            "Autonomous Agent Harness & Supervisor",
            muted_style,
            area.width - 4,
        );

        let div_line = "─".repeat((area.width.saturating_sub(4)) as usize);
        put_str(
            buf,
            area.x + 2,
            area.y + 3,
            &div_line,
            muted_style,
            area.width - 4,
        );

        match self.screen {
            WizardScreen::Agents => {
                put_str(
                    buf,
                    area.x + 2,
                    area.y + 5,
                    "Step 1: Select Coding Agents to Supervise",
                    title_style,
                    area.width - 4,
                );
                put_str(
                    buf,
                    area.x + 2,
                    area.y + 6,
                    "Kobold detected existing agents on your machine. Choose which agents Kobold can supervise:",
                    muted_style,
                    area.width - 4,
                );

                // Row 0: Select All
                let row0_y = area.y + 8;
                let select_all_mark = if self.all_agents_selected() {
                    "[x]"
                } else {
                    "[ ]"
                };
                let is_sel0 = self.selected_agent_row == 0;
                let row0_text = format!("{} (Select all / Deselect all)", select_all_mark);
                put_str(
                    buf,
                    area.x + 4,
                    row0_y,
                    &row0_text,
                    if is_sel0 { cursor_style } else { title_style },
                    area.width - 8,
                );

                // Rows 1..=5: Agents
                for (i, agent) in self.detected_agents.iter().enumerate() {
                    let y = area.y + 10 + (i as u16);
                    let checked = self.agents_checked.get(i).copied().unwrap_or(false);
                    let check_mark = if checked { "[x]" } else { "[ ]" };
                    let is_cursor = self.selected_agent_row == i + 1;

                    let status_desc = if let Some(ref path) = agent.path {
                        let ver = agent.version.as_deref().unwrap_or("detected");
                        format!("(found: {} - {})", path.display(), ver)
                    } else {
                        "(not found on PATH)".to_string()
                    };

                    let line_text =
                        format!("{} {:<14} {}", check_mark, agent.spec.name, status_desc);
                    let style = if is_cursor {
                        cursor_style
                    } else if agent.path.is_some() {
                        normal_style
                    } else {
                        muted_style
                    };
                    put_str(buf, area.x + 4, y, &line_text, style, area.width - 8);
                }

                // Next Button
                let next_y = area.y + 11 + (self.detected_agents.len() as u16);
                let is_next_cursor = self.selected_agent_row == self.detected_agents.len() + 1;
                let next_btn = "  [ Next -> ]  ";
                put_str(
                    buf,
                    area.x + 4,
                    next_y + 1,
                    next_btn,
                    if is_next_cursor {
                        btn_active_style
                    } else {
                        btn_inactive_style
                    },
                    area.width - 8,
                );

                // Footer hints
                let footer_y = area.height.saturating_sub(2);
                put_str(
                    buf,
                    area.x + 2,
                    footer_y,
                    "[Up/Down] Navigate  [Space] Toggle checkbox  [Enter/n] Next  [Ctrl-C] Exit",
                    muted_style,
                    area.width - 4,
                );
            }
            WizardScreen::Providers => {
                put_str(
                    buf,
                    area.x + 2,
                    area.y + 5,
                    "Step 2: Select Model Providers & LLM Gateways",
                    title_style,
                    area.width - 4,
                );
                put_str(
                    buf,
                    area.x + 2,
                    area.y + 6,
                    "Do you also want Kobold to connect directly to one or more model providers?",
                    muted_style,
                    area.width - 4,
                );

                let providers = [
                    (
                        "OpenAI",
                        self.provider_checked[0],
                        "Direct Realtime & Responses WebSocket adapter",
                        true,
                    ),
                    (
                        "OpenRouter",
                        self.provider_checked[1],
                        "Multi-model Gateway with PKCE OAuth authorization",
                        true,
                    ),
                    ("Anthropic", false, "(disabled, coming soon)", false),
                    ("Google", false, "(disabled, coming soon)", false),
                ];

                for (i, (name, checked, desc, enabled)) in providers.iter().enumerate() {
                    let y = area.y + 8 + (i as u16 * 2);
                    let is_cursor = self.selected_provider_row == i;
                    let check_mark = if *checked {
                        "[x]"
                    } else if *enabled {
                        "[ ]"
                    } else {
                        "[-]"
                    };

                    let line_text = format!("{} {:<12} - {}", check_mark, name, desc);
                    let style = if is_cursor {
                        cursor_style
                    } else if *enabled {
                        normal_style
                    } else {
                        muted_style
                    };
                    put_str(buf, area.x + 4, y, &line_text, style, area.width - 8);
                }

                // Buttons: Back and Next
                let btn_y = area.y + 17;
                let is_btn_row = self.selected_provider_row == 4;
                let back_active = is_btn_row && self.provider_button_focus == 0;
                let next_active = is_btn_row && self.provider_button_focus == 1;

                put_str(
                    buf,
                    area.x + 4,
                    btn_y,
                    "  [ <- Back ]  ",
                    if back_active {
                        btn_active_style
                    } else {
                        btn_inactive_style
                    },
                    20,
                );
                put_str(
                    buf,
                    area.x + 22,
                    btn_y,
                    "  [ Next -> ]  ",
                    if next_active {
                        btn_active_style
                    } else {
                        btn_inactive_style
                    },
                    20,
                );

                // Footer hints
                let footer_y = area.height.saturating_sub(2);
                put_str(
                    buf,
                    area.x + 2,
                    footer_y,
                    "[Up/Down/Left/Right] Navigate  [Space] Toggle  [Enter] Select  [b] Back  [n] Next",
                    muted_style,
                    area.width - 4,
                );
            }
            WizardScreen::Done => {}
        }

        Painted::default()
    }
}

fn put_str(buf: &mut Buffer, x: u16, y: u16, text: &str, style: Style, max_w: u16) {
    for (cx, ch) in (x..).zip(text.chars()) {
        if cx >= x + max_w || cx >= buf.area.width || y >= buf.area.height {
            break;
        }
        buf[(cx, y)].set_char(ch).set_style(style);
    }
}

/// Execute the full first-run onboarding wizard.
pub async fn run_onboarding_wizard(root: &Path) -> Result<Settings, Box<dyn std::error::Error>> {
    // 1. Detect agents on host
    let detected = crate::doctor::detect_all_agents().await;
    let mut state = WizardState::new(detected);

    // 2. Interactive TUI Loop
    {
        let mut screen = Screen::init()?;
        crate::term::enable_key_disambiguation();
        let mut event_stream = crossterm::event::EventStream::new();

        screen.draw(|buf, area| state.render(buf, area))?;

        while state.screen != WizardScreen::Done {
            let ev = event_stream.next().await;
            match ev {
                Some(Ok(Event::Key(k))) => {
                    if k.kind == KeyEventKind::Press {
                        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
                        let cont = state.handle_key(k.code, ctrl);
                        if !cont {
                            crate::term::restore();
                            std::process::exit(0);
                        }
                        screen.draw(|buf, area| state.render(buf, area))?;
                    }
                }
                Some(Ok(Event::Resize(..))) => {
                    screen.draw(|buf, area| state.render(buf, area))?;
                }
                _ => {}
            }
        }
    }

    // Terminal restored cleanly for step-by-step stdout operations
    crate::term::restore();

    println!("\n\x1b[1;32m════════════════════════════════════════════════════════════════\x1b[0m");
    println!("\x1b[1mKobold First-Run Setup: Finalizing Configuration\x1b[0m");
    println!("\x1b[1;32m════════════════════════════════════════════════════════════════\x1b[0m\n");

    // Gather user selections
    let mut selected_agent_ids = Vec::new();
    let mut selected_agent_specs = Vec::new();
    for (i, &checked) in state.agents_checked.iter().enumerate() {
        if checked {
            let spec = &state.detected_agents[i].spec;
            selected_agent_ids.push(spec.id.to_string());
            selected_agent_specs.push(spec.clone());
        }
    }

    let mut selected_providers = Vec::new();
    if state.provider_checked[0] {
        selected_providers.push("openai".to_string());
    }
    if state.provider_checked[1] {
        selected_providers.push("openrouter".to_string());
    }

    // 3. Download and install only the required adapters
    println!("\x1b[1mPhase 1/3: Installing Required Adapters...\x1b[0m");
    let installed_adapters =
        install_required_adapters(&selected_agent_ids, &selected_providers).await?;
    if installed_adapters.is_empty() {
        println!("All chosen adapters are already installed on this system.\n");
    } else {
        println!("Installed adapters: {}\n", installed_adapters.join(", "));
    }

    // 4. Configure credentials (OAuth PKCE for OpenRouter, prompt/link for OpenAI)
    println!("\x1b[1mPhase 2/3: Authenticating Providers...\x1b[0m");
    let mut openrouter_key = String::new();
    let mut openai_key = String::new();

    if state.provider_checked[1] {
        match openrouter_oauth_flow().await {
            Ok(key) => {
                println!("\x1b[32m✓\x1b[0m OpenRouter successfully authenticated.");
                openrouter_key = key;
            }
            Err(e) => {
                eprintln!("\x1b[33mwarning:\x1b[0m OpenRouter OAuth failed: {e}. You can configure it later in .kobold/settings.json.");
            }
        }
    }

    if state.provider_checked[0] {
        match prompt_openai_key() {
            Ok(key) => {
                openai_key = key;
            }
            Err(e) => {
                eprintln!("\x1b[33mwarning:\x1b[0m Could not read OpenAI key: {e}.");
            }
        }
    }

    // 5. Run parallel Agent Doctor verification
    println!("\n\x1b[1mPhase 3/3: Running Agent Sanity Checks in Parallel...\x1b[0m");
    let health_results = if !selected_agent_specs.is_empty() {
        check_all_agents(&selected_agent_specs, Duration::from_secs(12)).await
    } else {
        Vec::new()
    };
    print_doctor_report(&health_results);

    // 6. Build and save settings.json
    let mut settings = Settings::default();

    // Determine default Southbound adapter
    if !selected_agent_ids.is_empty() {
        settings.adapter = "kobold-adapter-acp".to_string();
    } else if state.provider_checked[0] || state.provider_checked[1] {
        settings.adapter = "kobold-openai".to_string();
    }

    // Save agents state
    for h in &health_results {
        settings.agents.insert(
            h.id.clone(),
            AgentSetting {
                name: h.name.clone(),
                command: h.command.clone(),
                enabled: h.detected && h.working,
                detected: h.detected,
                working: h.working,
                status: h.status.clone(),
            },
        );
    }

    // Save providers state
    if state.provider_checked[0] {
        settings.providers.insert(
            "openai".to_string(),
            ProviderSetting {
                name: "OpenAI".to_string(),
                enabled: true,
                api_key: openai_key,
                base_url: None,
            },
        );
    }

    if state.provider_checked[1] {
        settings.providers.insert(
            "openrouter".to_string(),
            ProviderSetting {
                name: "OpenRouter".to_string(),
                enabled: true,
                api_key: openrouter_key,
                base_url: Some("https://openrouter.ai/api/v1".to_string()),
            },
        );
    }

    settings.save(root)?;
    println!(
        "\x1b[32m✓\x1b[0m Setup complete! Settings saved to \x1b[1m{}/.kobold/settings.json\x1b[0m\n",
        root.display()
    );

    Ok(settings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doctor::SUPPORTED_AGENTS;

    #[test]
    fn wizard_toggle_select_all() {
        let spec = SUPPORTED_AGENTS[0].clone();
        let detected = vec![(spec, Some(PathBuf::from("/bin/claude")), None)];
        let mut state = WizardState::new(detected);

        assert_eq!(state.agents_checked, vec![true]);
        assert!(state.all_agents_selected());

        state.toggle_select_all();
        assert_eq!(state.agents_checked, vec![false]);
        assert!(!state.all_agents_selected());

        state.toggle_select_all();
        assert_eq!(state.agents_checked, vec![true]);
        assert!(state.all_agents_selected());
    }

    #[test]
    fn wizard_navigation_keys() {
        let spec = SUPPORTED_AGENTS[0].clone();
        let detected = vec![(spec, None, None)];
        let mut state = WizardState::new(detected);

        assert_eq!(state.selected_agent_row, 0);
        state.handle_key(KeyCode::Down, false);
        assert_eq!(state.selected_agent_row, 1);
        state.handle_key(KeyCode::Char(' '), false);
        assert_eq!(state.agents_checked, vec![true]);

        state.handle_key(KeyCode::Char('n'), false);
        assert_eq!(state.screen, WizardScreen::Providers);
    }
}
