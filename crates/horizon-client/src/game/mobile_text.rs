//! Native iPad text and touch routing. The platform layer sends committed
//! text only; each screen keeps its own filtering, selection and key behavior.
use super::{Game, Loading, Screen, startup_menu::MobileTouchAction};
use horizon_client::Result;
use winit::{
    dpi::PhysicalPosition,
    event::{DeviceId, ElementState, MouseButton, Touch, WindowEvent},
    keyboard::KeyCode,
};

impl Game<'_> {
    /// Insert committed native text into the active text field.
    pub(super) fn mobile_text(&mut self, text: &str) -> Result<()> {
        if text.is_empty() || !self.mobile_wants_keyboard() {
            return Ok(());
        }
        if self.screen == Screen::Chat && self.title.is_none() {
            // AltLeft is an inert key for ChatScreen; the existing handler
            // still applies chat filtering, completion and suggestion updates.
            self.chat_key_code(KeyCode::AltLeft, Some(text));
        } else if self.sign_edit.is_some() {
            self.sign_input(None, Some(text));
        } else if self.book.is_some() {
            self.book_input(None, Some(text));
        } else if self.screen == Screen::Creative && self.creative_screen.tab().search {
            self.creative_search_edit(Some(text), false);
        } else if self.recipe_search_focused() {
            self.recipe_text(Some(text), false);
        } else if self.station_typing() {
            self.station_text(Some(text), false);
        } else if self.screen == Screen::WorldOptions {
            self.world_options_text(Some(text));
        } else if self.screen == Screen::CommandBlock {
            self.command_block_type(text);
        } else if self.screen == Screen::Options {
            self.ui.options.type_text(&self.gui.atlas, text);
        } else {
            self.title_edit(Some(text), false);
        }
        Ok(())
    }

    /// Delete one character in the active native text field.
    pub(super) fn mobile_backspace(&mut self) -> Result<()> {
        if !self.mobile_wants_keyboard() {
            return Ok(());
        }
        if self.screen == Screen::Chat && self.title.is_none() {
            self.chat_key_code(KeyCode::Backspace, None);
        } else if self.sign_edit.is_some() {
            self.sign_input(Some(KeyCode::Backspace), None);
        } else if self.book.is_some() {
            self.book_input(Some(KeyCode::Backspace), None);
        } else if self.screen == Screen::Creative && self.creative_screen.tab().search {
            self.creative_search_edit(None, true);
        } else if self.recipe_search_focused() {
            self.recipe_text(None, true);
        } else if self.station_typing() {
            self.station_text(None, true);
        } else if self.screen == Screen::WorldOptions {
            self.world_options_text(None);
        } else if self.screen == Screen::CommandBlock {
            self.command_block_mobile_backspace();
        } else if self.screen == Screen::Options {
            self.options_key(KeyCode::Backspace, None)?;
        } else {
            self.title_edit(None, true);
        }
        Ok(())
    }

    /// Enter follows the active screen's existing submit/advance behavior.
    pub(super) fn mobile_enter(&mut self) -> Result<()> {
        if !self.mobile_wants_keyboard() {
            return Ok(());
        }
        if self.screen == Screen::Chat && self.title.is_none() {
            self.chat_key_code(KeyCode::Enter, None);
        } else if self.sign_edit.is_some() {
            self.sign_input(Some(KeyCode::Enter), None);
        } else if self.book.is_some() {
            self.book_input(Some(KeyCode::Enter), None);
        } else if self.screen == Screen::CommandBlock {
            self.command_block_mobile_enter();
        } else if self.screen == Screen::Options {
            self.options_key(KeyCode::Enter, None)?;
        } else if self.title.is_some() {
            if let Some(title) = &mut self.title {
                title.confirm_edit();
            }
        }
        Ok(())
    }

    /// True only while a real editable field owns focus.
    pub(super) fn mobile_wants_keyboard(&mut self) -> bool {
        if self.screen == Screen::Chat && self.title.is_none() {
            return self.ui.chat_screen.is_some();
        }
        if self.sign_edit.is_some() {
            return true;
        }
        if self
            .book
            .as_ref()
            .is_some_and(|book| matches!(book, super::books::BookScreen::Edit { .. }))
        {
            return true;
        }
        match self.screen {
            Screen::Creative => return self.creative_screen.tab().search,
            Screen::WorldOptions => return self.world_options_mobile_text_focused(),
            Screen::CommandBlock => return self.command_block_screen.is_some(),
            Screen::Options => return self.ui.options.mobile_text_focused(),
            _ => {}
        }
        if self.recipe_search_focused() || self.station_typing() {
            return true;
        }
        self.title
            .as_mut()
            .is_some_and(|title| title.focused_field().is_some())
    }
}

impl Loading {
    /// Route one native finger through the same cursor and click handlers the
    /// loading menu uses for a mouse. The startup menu owns one drag finger.
    pub(super) fn mobile_touch(
        &mut self,
        event_loop: &winit::event_loop::ActiveEventLoop,
        touch: &Touch,
    ) -> Result<()> {
        if self.gui.is_none() && touch.phase != winit::event::TouchPhase::Cancelled {
            return Ok(());
        }
        for action in
            self.menu
                .mobile_touch(touch.id, touch.phase, [touch.location.x, touch.location.y])
        {
            match action {
                MobileTouchAction::Move([x, y]) => self.menu_event(
                    event_loop,
                    WindowEvent::CursorMoved {
                        device_id: DeviceId::dummy(),
                        position: PhysicalPosition::new(x, y),
                    },
                )?,
                MobileTouchAction::Button(state) => self.menu_event(
                    event_loop,
                    WindowEvent::MouseInput {
                        device_id: DeviceId::dummy(),
                        state,
                        button: MouseButton::Left,
                    },
                )?,
                MobileTouchAction::Cancelled => self.mobile_cancel_touch_drag(),
            }
        }
        Ok(())
    }

    pub(super) fn mobile_text(&mut self, text: &str) -> Result<()> {
        if let Some(gui) = self.gui.as_ref() {
            self.menu.mobile_text(&gui.atlas, text);
        }
        self.save_menu_address();
        Ok(())
    }

    pub(super) fn mobile_backspace(&mut self) -> Result<()> {
        if let Some(gui) = self.gui.as_ref() {
            let events = self.menu.mobile_backspace(&mut self.settings, &gui.atlas);
            self.menu_option_events(events);
        }
        self.save_menu_address();
        Ok(())
    }

    pub(super) fn mobile_enter(&mut self) -> Result<()> {
        if let Some(gui) = self.gui.as_ref() {
            let events = self.menu.mobile_enter(&mut self.settings, &gui.atlas);
            self.menu_option_events(events);
        }
        self.save_menu_address();
        Ok(())
    }

    pub(super) fn mobile_wants_keyboard(&mut self) -> bool {
        self.menu.mobile_wants_keyboard()
    }
}
