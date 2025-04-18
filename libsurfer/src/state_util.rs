//! Utility functions, typically inlined, for more readable code

use ecolor::Color32;

use crate::{displayed_item::DisplayedItem, SystemState};

impl SystemState {
    #[inline]
    pub fn get_item_text_color(&self, item: &DisplayedItem) -> &Color32 {
        item.color()
            .and_then(|color| self.user.config.theme.get_color(color))
            .unwrap_or(&self.user.config.theme.primary_ui_color.foreground)
    }

    #[inline]
    pub fn show_statusbar(&self) -> bool {
        self.user.show_statusbar.unwrap_or_else(|| {
            (self.user.waves.is_some() || self.progress_tracker.is_some())
                && self.user.config.layout.show_statusbar()
        })
    }

    #[inline]
    pub fn show_toolbar(&self) -> bool {
        self.user
            .show_toolbar
            .unwrap_or_else(|| self.user.config.layout.show_toolbar())
    }

    #[inline]
    pub fn show_overview(&self) -> bool {
        self.user
            .show_overview
            .unwrap_or_else(|| self.user.config.layout.show_overview())
    }

    #[inline]
    pub fn show_hierarchy(&self) -> bool {
        self.user
            .show_hierarchy
            .unwrap_or_else(|| self.user.config.layout.show_hierarchy())
    }

    #[inline]
    pub fn show_tooltip(&self) -> bool {
        self.user
            .show_tooltip
            .unwrap_or_else(|| self.user.config.layout.show_tooltip())
    }

    #[inline]
    pub fn show_scope_tooltip(&self) -> bool {
        self.user
            .show_scope_tooltip
            .unwrap_or_else(|| self.user.config.layout.show_scope_tooltip())
    }

    #[inline]
    pub fn show_ticks(&self) -> bool {
        self.user
            .show_ticks
            .unwrap_or_else(|| self.user.config.layout.show_ticks())
    }

    #[inline]
    pub fn show_menu(&self) -> bool {
        self.user
            .show_menu
            .unwrap_or_else(|| self.user.config.layout.show_menu())
    }

    #[inline]
    pub fn show_variable_indices(&self) -> bool {
        self.user
            .show_variable_indices
            .unwrap_or_else(|| self.user.config.layout.show_variable_indices())
    }

    #[inline]
    pub fn show_variable_direction(&self) -> bool {
        self.user
            .show_variable_direction
            .unwrap_or_else(|| self.user.config.layout.show_variable_direction())
    }

    #[inline]
    pub fn ui_zoom_factor(&self) -> f32 {
        self.user
            .ui_zoom_factor
            .unwrap_or_else(|| self.user.config.layout.default_zoom_factor())
    }

    #[inline]
    pub fn show_empty_scopes(&self) -> bool {
        self.user
            .show_empty_scopes
            .unwrap_or_else(|| self.user.config.layout.show_empty_scopes())
    }

    #[inline]
    pub fn show_parameters_in_scopes(&self) -> bool {
        self.user
            .show_parameters_in_scopes
            .unwrap_or_else(|| self.user.config.layout.show_parameters_in_scopes())
    }

    #[inline]
    pub fn show_default_timeline(&self) -> bool {
        self.user
            .show_default_timeline
            .unwrap_or_else(|| self.user.config.layout.show_default_timeline())
    }

    #[inline]
    pub fn highlight_focused(&self) -> bool {
        self.user
            .highlight_focused
            .unwrap_or_else(|| self.user.config.layout.highlight_focused())
    }

    #[inline]
    pub fn fill_high_values(&self) -> bool {
        self.user
            .fill_high_values
            .unwrap_or_else(|| self.user.config.layout.fill_high_values())
    }
}
