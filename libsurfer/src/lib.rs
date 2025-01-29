#![deny(unused_crate_dependencies)]

#[cfg(feature = "performance_plot")]
pub mod benchmark;
mod channels;
pub mod clock_highlighting;
pub mod command_prompt;
pub mod config;
pub mod cxxrtl;
pub mod cxxrtl_container;
pub mod data_container;
pub mod dialog;
pub mod displayed_item;
pub mod displayed_item_tree;
pub mod drawing_canvas;
pub mod file_watcher;
pub mod graphics;
pub mod help;
pub mod hierarchy;
pub mod keys;
pub mod logs;
pub mod marker;
pub mod menus;
pub mod message;
pub mod mousegestures;
pub mod overview;
pub mod remote;
pub mod state;
pub mod state_util;
pub mod statusbar;
pub mod system_state;
#[cfg(test)]
pub mod tests;
pub mod time;
pub mod toolbar;
pub mod transaction_container;
pub mod translation;
pub mod util;
pub mod variable_direction;
mod variable_index;
pub mod variable_name_filter;
pub mod variable_name_type;
pub mod variable_type;
pub mod view;
pub mod viewport;
#[cfg(target_arch = "wasm32")]
pub mod wasm_api;
#[cfg(target_arch = "wasm32")]
pub mod wasm_panic;
pub mod wasm_util;
pub mod wave_container;
pub mod wave_data;
pub mod wave_source;
pub mod wcp;
pub mod wellen;

use std::collections::HashMap;
use std::sync::atomic::AtomicU32;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, RwLock};

use camino::Utf8PathBuf;
use channels::{GlobalChannelTx, IngressHandler, IngressReceiver};
use color_eyre::eyre::Context;
use color_eyre::Result;
use derive_more::Display;
use displayed_item::DisplayedVariable;
use displayed_item_tree::DisplayedItemTree;
use eframe::{App, CreationContext};
use egui::{FontData, FontDefinitions, FontFamily};
use ftr_parser::types::Transaction;
use futures::executor::block_on;
use itertools::Itertools;
use lazy_static::lazy_static;
use log::{error, info, warn};
use num::BigInt;
use serde::Deserialize;
pub use state::State;
use surfer_translation_types::Translator;
use wcp::{proto::WcpCSMessage, proto::WcpEvent, proto::WcpSCMessage};

use crate::config::{SurferConfig, SurferTheme};
use crate::dialog::ReloadWaveformDialog;
use crate::displayed_item::{DisplayedFieldRef, DisplayedItem, DisplayedItemRef, FieldFormat};
use crate::displayed_item_tree::VisibleItemIndex;
use crate::drawing_canvas::TxDrawingCommands;
use crate::message::{HeaderResult, Message};
use crate::transaction_container::{StreamScopeRef, TransactionRef, TransactionStreamRef};
#[cfg(feature = "spade")]
use crate::translation::spade::SpadeTranslator;
use crate::translation::{all_translators, AnyTranslator};
use crate::variable_name_filter::VariableNameFilterType;
use crate::viewport::Viewport;
use crate::wasm_util::{perform_work, UrlArgs};
use crate::wave_container::{ScopeRefExt, WaveContainer};
use crate::wave_data::{ScopeType, WaveData};
use crate::wave_source::{LoadOptions, WaveFormat, WaveSource};
use crate::wellen::convert_format;

lazy_static! {
    pub static ref EGUI_CONTEXT: RwLock<Option<Arc<egui::Context>>> = RwLock::new(None);
    /// A number that is non-zero if there are asynchronously triggered operations that
    /// have been triggered but not successfully completed yet. In practice, if this is
    /// non-zero, we will re-run the egui update function in order to ensure that we deal
    /// with the outstanding transactions eventually.
    /// When incrementing this, it is important to make sure that it gets decremented
    /// whenever the asynchronous transaction is completed, otherwise we will re-render
    /// things until program exit
    pub(crate) static ref OUTSTANDING_TRANSACTIONS: AtomicU32 = AtomicU32::new(0);

    pub(crate) static ref WCP_CS_HANDLER: IngressHandler<WcpCSMessage> = IngressHandler::new();
    pub(crate) static ref WCP_SC_HANDLER: GlobalChannelTx<WcpSCMessage> = GlobalChannelTx::new();
}

pub struct StartupParams {
    pub spade_state: Option<Utf8PathBuf>,
    pub spade_top: Option<String>,
    pub waves: Option<WaveSource>,
    pub startup_commands: Vec<String>,
}

impl StartupParams {
    #[allow(dead_code)] // NOTE: Only used in wasm version
    pub fn empty() -> Self {
        Self {
            spade_state: None,
            spade_top: None,
            waves: None,
            startup_commands: vec![],
        }
    }

    #[allow(dead_code)] // NOTE: Only used in wasm version
    pub fn from_url(url: UrlArgs) -> Self {
        Self {
            spade_state: None,
            spade_top: None,
            waves: url.load_url.map(WaveSource::Url),
            startup_commands: url.startup_commands.map(|c| vec![c]).unwrap_or_default(),
        }
    }
}

fn setup_custom_font(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();

    fonts.font_data.insert(
        "remix_icons".to_owned(),
        FontData::from_static(egui_remixicon::FONT).into(),
    );

    fonts
        .families
        .get_mut(&FontFamily::Proportional)
        .unwrap()
        .push("remix_icons".to_owned());

    ctx.set_fonts(fonts);
}

pub fn run_egui(cc: &CreationContext, mut state: State) -> Result<Box<dyn App>> {
    let ctx_arc = Arc::new(cc.egui_ctx.clone());
    *EGUI_CONTEXT.write().unwrap() = Some(ctx_arc.clone());
    state.sys.context = Some(ctx_arc.clone());
    cc.egui_ctx
        .set_visuals_of(egui::Theme::Dark, state.get_visuals());
    cc.egui_ctx
        .set_visuals_of(egui::Theme::Light, state.get_visuals());
    #[cfg(not(target_arch = "wasm32"))]
    if state.config.wcp.autostart {
        state.start_wcp_server(Some(state.config.wcp.address.clone()), false);
    }
    setup_custom_font(&cc.egui_ctx);
    Ok(Box::new(state))
}

#[derive(Debug, Clone, Copy, Deserialize, Display, PartialEq, Eq)]
pub enum MoveDir {
    #[display("up")]
    Up,

    #[display("down")]
    Down,
}

pub enum ColorSpecifier {
    Index(usize),
    Name(String),
}

enum CachedDrawData {
    WaveDrawData(CachedWaveDrawData),
    TransactionDrawData(CachedTransactionDrawData),
}

struct CachedWaveDrawData {
    pub draw_commands: HashMap<DisplayedFieldRef, drawing_canvas::DrawingCommands>,
    pub clock_edges: Vec<f32>,
    pub ticks: Vec<(String, f32)>,
}

struct CachedTransactionDrawData {
    pub draw_commands: HashMap<TransactionRef, TxDrawingCommands>,
    pub stream_to_displayed_txs: HashMap<TransactionStreamRef, Vec<TransactionRef>>,
    pub inc_relation_tx_ids: Vec<TransactionRef>,
    pub out_relation_tx_ids: Vec<TransactionRef>,
}

pub struct Channels {
    pub msg_sender: Sender<Message>,
    pub msg_receiver: Receiver<Message>,
    wcp_c2s_receiver: Option<IngressReceiver<WcpCSMessage>>,
    wcp_s2c_sender: Option<tokio::sync::mpsc::Sender<WcpSCMessage>>,
}
impl Channels {
    fn new() -> Self {
        let (msg_sender, msg_receiver) = mpsc::channel();
        Self {
            msg_sender,
            msg_receiver,
            wcp_c2s_receiver: None,
            wcp_s2c_sender: None,
        }
    }
}

/// Stores the current canvas state to enable undo/redo operations
struct CanvasState {
    message: String,
    focused_item: Option<VisibleItemIndex>,
    focused_transaction: (Option<TransactionRef>, Option<Transaction>),
    items_tree: DisplayedItemTree,
    displayed_items: HashMap<DisplayedItemRef, DisplayedItem>,
    markers: HashMap<u8, BigInt>,
}

impl State {
    pub fn update(&mut self, message: Message) {
        if log::log_enabled!(log::Level::Trace)
            && !matches!(message, Message::CommandPromptUpdate { .. })
        {
            let mut s = format!("{message:?}");
            s.shrink_to(100);
            log::info!("{s}");
        }
        match message {
            Message::SetActiveScope(scope) => {
                let Some(waves) = self.waves.as_mut() else {
                    return;
                };
                let scope = if let ScopeType::StreamScope(StreamScopeRef::Empty(name)) = scope {
                    ScopeType::StreamScope(StreamScopeRef::new_stream_from_name(
                        waves.inner.as_transactions().unwrap(),
                        name,
                    ))
                } else {
                    scope
                };

                if waves.inner.scope_exists(&scope) {
                    waves.active_scope = Some(scope);
                } else {
                    warn!("Setting active scope to {scope} which does not exist");
                }
            }
            Message::AddVariables(vars) => {
                if !vars.is_empty() {
                    let undo_msg = if vars.len() == 1 {
                        format!("Add variable {}", vars[0].name)
                    } else {
                        format!("Add {} variables", vars.len())
                    };
                    self.save_current_canvas(undo_msg);
                    if let Some(waves) = self.waves.as_mut() {
                        if let (Some(cmd), _) =
                            waves.add_variables(&self.sys.translators, vars, None)
                        {
                            self.load_variables(cmd);
                        }
                        self.invalidate_draw_commands();
                    } else {
                        error!("Could not load signals, no waveform loaded");
                    }
                }
            }
            Message::AddDivider(name, vidx) => {
                self.save_current_canvas("Add divider".into());
                if let Some(waves) = self.waves.as_mut() {
                    waves.add_divider(name, vidx);
                }
            }
            Message::AddTimeLine(vidx) => {
                self.save_current_canvas("Add timeline".into());
                if let Some(waves) = self.waves.as_mut() {
                    waves.add_timeline(vidx);
                }
            }
            Message::AddScope(scope, recursive) => {
                self.save_current_canvas(format!("Add scope {}", scope.name()));
                self.add_scope(scope, recursive);
            }
            Message::AddCount(digit) => {
                if let Some(count) = &mut self.count {
                    count.push(digit);
                } else {
                    self.count = Some(digit.to_string());
                }
            }
            Message::AddStreamOrGenerator(s) => {
                let undo_msg = if let Some(gen_id) = s.gen_id {
                    format!("Add generator(id: {})", gen_id)
                } else {
                    format!("Add stream(id: {})", s.stream_id)
                };
                self.save_current_canvas(undo_msg);

                if let Some(waves) = self.waves.as_mut() {
                    if s.gen_id.is_some() {
                        waves.add_generator(s);
                    } else {
                        waves.add_stream(s);
                    }
                    self.invalidate_draw_commands();
                }
            }
            Message::AddStreamOrGeneratorFromName(scope, name) => {
                self.save_current_canvas(format!(
                    "Add Stream/Generator from name: {}",
                    name.clone()
                ));
                if let Some(waves) = self.waves.as_mut() {
                    let Some(inner) = waves.inner.as_transactions() else {
                        return;
                    };
                    if let Some(scope) = scope {
                        match scope {
                            StreamScopeRef::Root => {
                                let (stream_id, name) = inner
                                    .get_stream_from_name(name)
                                    .map(|s| (s.id, s.name.clone()))
                                    .unwrap();

                                waves.add_stream(TransactionStreamRef::new_stream(stream_id, name));
                            }
                            StreamScopeRef::Stream(stream) => {
                                let (stream_id, id, name) = inner
                                    .get_generator_from_name(Some(stream.stream_id), name)
                                    .map(|gen| (gen.stream_id, gen.id, gen.name.clone()))
                                    .unwrap();

                                waves.add_generator(TransactionStreamRef::new_gen(
                                    stream_id, id, name,
                                ));
                            }
                            StreamScopeRef::Empty(_) => {}
                        }
                    } else {
                        let (stream_id, id, name) = inner
                            .get_generator_from_name(None, name)
                            .map(|gen| (gen.stream_id, gen.id, gen.name.clone()))
                            .unwrap();

                        waves.add_generator(TransactionStreamRef::new_gen(stream_id, id, name));
                    }
                    self.invalidate_draw_commands();
                }
            }
            Message::AddAllFromStreamScope(scope_name) => {
                self.save_current_canvas(format!("Add all from scope {}", scope_name.clone()));
                if let Some(waves) = self.waves.as_mut() {
                    if scope_name == "tr" {
                        waves.add_all_streams();
                    } else {
                        let Some(inner) = waves.inner.as_transactions() else {
                            return;
                        };
                        if let Some(stream) = inner.get_stream_from_name(scope_name) {
                            let gens = stream
                                .generators
                                .iter()
                                .map(|gen_id| inner.get_generator(*gen_id).unwrap())
                                .map(|gen| (gen.stream_id, gen.id, gen.name.clone()))
                                .collect_vec();

                            for (stream_id, id, name) in gens {
                                waves.add_generator(TransactionStreamRef::new_gen(
                                    stream_id,
                                    id,
                                    name.clone(),
                                ))
                            }
                        }
                    }
                    self.invalidate_draw_commands();
                }
            }
            Message::InvalidateCount => self.count = None,
            Message::SetNameAlignRight(align_right) => {
                self.align_names_right = Some(align_right);
            }
            Message::FocusItem(idx) => {
                let Some(waves) = self.waves.as_mut() else {
                    return;
                };

                let visible_items_len = waves.displayed_items.len();
                if visible_items_len > 0 && idx.0 < visible_items_len {
                    waves.focused_item = Some(idx);
                } else {
                    error!(
                        "Can not focus variable {} because only {visible_items_len} variables are visible.", idx.0
                    );
                }
            }
            Message::ItemSelectRange(select_to) => {
                let Some(waves) = self.waves.as_mut() else {
                    return;
                };

                if let Some(select_from) = waves.focused_item {
                    waves
                        .items_tree
                        .xselect_visible_range(select_from, select_to, true);
                }
            }
            Message::ToggleItemSelected(vidx) => {
                let Some(waves) = self.waves.as_mut() else {
                    return;
                };
                if let Some(node) = vidx
                    .or(waves.focused_item)
                    .and_then(|vidx| waves.items_tree.to_displayed(vidx))
                    .and_then(|item| waves.items_tree.get_mut(item))
                {
                    node.selected = !node.selected
                }
            }
            Message::ToggleDefaultTimeline => {
                self.config.layout.show_default_timeline = !self.config.layout.show_default_timeline
            }
            Message::UnfocusItem => {
                if let Some(waves) = self.waves.as_mut() {
                    waves.focused_item = None;
                };
            }
            Message::RenameItem(vidx) => {
                self.save_current_canvas(format!(
                    "Rename item to {}",
                    self.sys.item_renaming_string.borrow()
                ));
                let Some(waves) = self.waves.as_mut() else {
                    return;
                };
                let vidx = vidx.or(waves.focused_item);
                if let Some(vidx) = vidx {
                    self.rename_target = Some(vidx);
                    *self.sys.item_renaming_string.borrow_mut() = waves
                        .items_tree
                        .get_visible(vidx)
                        .and_then(|node| waves.displayed_items.get(&node.item_ref))
                        .map(displayed_item::DisplayedItem::name)
                        .unwrap_or_default();
                }
            }
            Message::MoveFocus(direction, count, select) => {
                let Some(waves) = self.waves.as_mut() else {
                    return;
                };
                let visible_item_cnt = waves.items_tree.iter_visible().count();
                if visible_item_cnt == 0 {
                    return;
                }

                let new_focus_vidx = VisibleItemIndex(match direction {
                    MoveDir::Up => waves
                        .focused_item
                        .map(|vidx| vidx.0)
                        .unwrap_or(visible_item_cnt)
                        .saturating_sub(count),
                    MoveDir::Down => waves
                        .focused_item
                        .map(|vidx| vidx.0)
                        .unwrap_or(usize::MAX)
                        .wrapping_add(count)
                        .clamp(0, visible_item_cnt - 1),
                });

                if select {
                    if let Some(idx) = waves
                        .focused_item
                        .and_then(|vidx| waves.items_tree.to_displayed(vidx))
                    {
                        waves.items_tree.xselect(idx, true)
                    };
                    if let Some(idx) = waves.items_tree.to_displayed(new_focus_vidx) {
                        waves.items_tree.xselect(idx, true)
                    }
                }
                waves.focused_item = Some(new_focus_vidx);
            }
            Message::FocusTransaction(tx_ref, tx) => {
                if tx_ref.is_some() && tx.is_none() {
                    self.save_current_canvas(format!(
                        "Focus Transaction id: {}",
                        tx_ref.as_ref().unwrap().id
                    ));
                }
                let Some(waves) = self.waves.as_mut() else {
                    return;
                };
                let invalidate = tx.is_none();
                waves.focused_transaction =
                    (tx_ref, tx.or_else(|| waves.focused_transaction.1.clone()));
                if invalidate {
                    self.invalidate_draw_commands();
                }
            }
            Message::ScrollToItem(position) => {
                if let Some(waves) = self.waves.as_mut() {
                    waves.scroll_to_item(position);
                }
            }
            Message::SetScrollOffset(offset) => {
                if let Some(waves) = self.waves.as_mut() {
                    waves.scroll_offset = offset;
                }
            }
            Message::SetLogsVisible(visibility) => self.show_logs = visibility,
            Message::SetCursorWindowVisible(visibility) => self.show_cursor_window = visibility,
            Message::VerticalScroll(direction, count) => {
                let Some(waves) = self.waves.as_mut() else {
                    return;
                };
                let current_item = waves.get_top_item();
                match direction {
                    MoveDir::Down => {
                        waves.scroll_to_item(current_item + count);
                    }
                    MoveDir::Up => {
                        if current_item > count {
                            waves.scroll_to_item(current_item - count);
                        } else {
                            waves.scroll_to_item(0);
                        }
                    }
                }
            }
            Message::RemoveItemByIndex(vidx) => {
                let waves = self.waves.as_ref();
                let item_ref = waves
                    .and_then(|waves| waves.items_tree.get_visible(vidx))
                    .map(|node| node.item_ref);
                let undo_msg = item_ref
                    .and_then(|item_ref| {
                        waves.and_then(|waves| waves.displayed_items.get(&item_ref))
                    })
                    .map(displayed_item::DisplayedItem::name)
                    .map(|name| format!("Remove item {name}"))
                    .unwrap_or("Remove one item".to_string());
                self.save_current_canvas(undo_msg);
                if let Some(waves) = self.waves.as_mut() {
                    if let Some(item_ref) = item_ref {
                        waves.remove_displayed_item(item_ref)
                    }
                };
            }
            Message::RemoveItems(mut items) => {
                let undo_msg = self
                    .waves
                    .as_ref()
                    .and_then(|waves| {
                        if items.len() == 1 {
                            items.first().and_then(|item_ref| {
                                waves
                                    .displayed_items
                                    .get(item_ref)
                                    .map(|item| format!("Remove item {}", item.name()))
                            })
                        } else {
                            Some(format!("Remove {} items", items.len()))
                        }
                    })
                    .unwrap_or("".to_string());
                self.save_current_canvas(undo_msg);
                let Some(waves) = self.waves.as_mut() else {
                    return;
                };

                items.sort();
                items.reverse(); // TODO do with sorting already...
                for id in items {
                    waves.remove_displayed_item(id);
                }
            }
            Message::MoveFocusedItem(direction, count) => {
                self.save_current_canvas(format!("Move item {direction}, {count}"));
                self.invalidate_draw_commands();
                let Some(waves) = self.waves.as_mut() else {
                    return;
                };
                let Some(vidx) = waves.focused_item else {
                    return;
                };
                let mut vidx = vidx;
                for _ in 0..count {
                    vidx = waves
                        .items_tree
                        .move_item(vidx, direction, |node| {
                            matches!(
                                waves.displayed_items.get(&node.item_ref),
                                Some(DisplayedItem::Group(..))
                            )
                        })
                        .expect("move failed for unknown reason");
                }
                waves.focused_item = waves.focused_item.and(Some(vidx));
            }
            Message::CanvasScroll {
                delta,
                viewport_idx,
            } => {
                if let Some(waves) = self.waves.as_mut() {
                    waves.viewports[viewport_idx]
                        .handle_canvas_scroll(delta.y as f64 + delta.x as f64);
                    self.invalidate_draw_commands();
                }
            }
            Message::CanvasZoom {
                delta,
                mouse_ptr,
                viewport_idx,
            } => {
                if let Some(waves) = self.waves.as_mut() {
                    let num_timestamps = waves
                        .num_timestamps()
                        .expect("No timestamps count, even though waveforms should be loaded");
                    waves.viewports[viewport_idx].handle_canvas_zoom(
                        mouse_ptr,
                        delta as f64,
                        &num_timestamps,
                    );
                    self.invalidate_draw_commands();
                }
            }
            Message::ZoomToFit { viewport_idx } => {
                if let Some(waves) = &mut self.waves {
                    waves.viewports[viewport_idx].zoom_to_fit();
                    self.invalidate_draw_commands();
                }
            }
            Message::GoToEnd { viewport_idx } => {
                if let Some(waves) = &mut self.waves {
                    waves.viewports[viewport_idx].go_to_end();
                    self.invalidate_draw_commands();
                }
            }
            Message::GoToStart { viewport_idx } => {
                if let Some(waves) = &mut self.waves {
                    waves.viewports[viewport_idx].go_to_start();
                    self.invalidate_draw_commands();
                }
            }
            Message::GoToTime(time, viewport_idx) => {
                if let Some(waves) = self.waves.as_mut() {
                    if let Some(time) = time {
                        let num_timestamps = waves
                            .num_timestamps()
                            .expect("No timestamps count, even though waveforms should be loaded");
                        waves.viewports[viewport_idx].go_to_time(&time.clone(), &num_timestamps);
                        self.invalidate_draw_commands();
                    }
                };
            }
            Message::SetTimeUnit(timeunit) => {
                self.wanted_timeunit = timeunit;
                self.invalidate_draw_commands();
            }
            Message::SetTimeStringFormatting(format) => {
                self.time_string_format = format;
                self.invalidate_draw_commands();
            }
            Message::ZoomToRange {
                start,
                end,
                viewport_idx,
            } => {
                if let Some(waves) = &mut self.waves {
                    let num_timestamps = waves
                        .num_timestamps()
                        .expect("No timestamps count, even though waveforms should be loaded");
                    waves.viewports[viewport_idx].zoom_to_range(&start, &end, &num_timestamps);
                    self.invalidate_draw_commands();
                }
            }
            Message::VariableFormatChange(displayed_field_ref, format) => {
                let Some(waves) = self.waves.as_mut() else {
                    return;
                };
                if !self
                    .sys
                    .translators
                    .all_translator_names()
                    .contains(&format.as_str())
                {
                    warn!("No translator {format}");
                    return;
                }

                let update_format =
                    |variable: &mut DisplayedVariable, field_ref: DisplayedFieldRef| {
                        if field_ref.field.is_empty() {
                            let Ok(meta) = waves
                                .inner
                                .as_waves()
                                .unwrap()
                                .variable_meta(&variable.variable_ref)
                                .map_err(|e| warn!("{e:#?}"))
                            else {
                                return;
                            };
                            let translator = self.sys.translators.get_translator(&format);
                            let new_info = translator.variable_info(&meta).unwrap();

                            variable.format = Some(format.clone());
                            variable.info = new_info;
                        } else {
                            variable
                                .field_formats
                                .retain(|ff| ff.field != field_ref.field);
                            variable.field_formats.push(FieldFormat {
                                field: field_ref.field,
                                format: format.clone(),
                            });
                        }
                    };

                // convert focused item index to item ref
                let focused = waves
                    .focused_item
                    .and_then(|vidx| waves.items_tree.get_visible(vidx))
                    .map(|node| node.item_ref);

                let mut redraw = false;

                if let Some(id @ DisplayedItemRef(_)) =
                    displayed_field_ref.as_ref().map(|r| r.item).or(focused)
                {
                    if let Some(DisplayedItem::Variable(displayed_variable)) =
                        waves.displayed_items.get_mut(&id)
                    {
                        update_format(displayed_variable, DisplayedFieldRef::from(id));
                    }
                    redraw = true;
                }
                if displayed_field_ref.is_none() {
                    for item in waves
                        .items_tree
                        .iter_visible_selected()
                        .map(|node| node.item_ref)
                    {
                        let field_ref = DisplayedFieldRef::from(item);
                        if let Some(DisplayedItem::Variable(variable)) =
                            waves.displayed_items.get_mut(&item)
                        {
                            update_format(variable, field_ref);
                        }
                    }
                    redraw = true;
                }

                if redraw {
                    self.invalidate_draw_commands();
                }
            }
            Message::ItemSelectionClear => {
                if let Some(waves) = self.waves.as_mut() {
                    waves.items_tree.xselect_all(false);
                }
            }
            Message::ItemColorChange(vidx, color_name) => {
                self.save_current_canvas(format!(
                    "Change item color to {}",
                    color_name.clone().unwrap_or("default".into())
                ));
                self.invalidate_draw_commands();
                if let Some(waves) = self.waves.as_mut() {
                    if let Some(vidx) = vidx.or(waves.focused_item) {
                        waves.items_tree.get_visible(vidx).map(|node| {
                            waves
                                .displayed_items
                                .entry(node.item_ref)
                                .and_modify(|item| item.set_color(color_name.clone()))
                        });
                    }
                    if vidx.is_none() {
                        for node in waves.items_tree.iter_visible_selected() {
                            waves
                                .displayed_items
                                .entry(node.item_ref)
                                .and_modify(|item| item.set_color(color_name.clone()));
                        }
                    }
                };
            }
            Message::ItemNameChange(vidx, name) => {
                self.save_current_canvas(format!(
                    "Change item name to {}",
                    name.clone().unwrap_or("default".into())
                ));
                if let Some(waves) = self.waves.as_mut() {
                    if let Some(vidx) = vidx.or(waves.focused_item) {
                        waves.items_tree.get_visible(vidx).map(|node| {
                            waves
                                .displayed_items
                                .entry(node.item_ref)
                                .and_modify(|item| item.set_name(name))
                        });
                    }
                };
            }
            Message::ItemBackgroundColorChange(vidx, color_name) => {
                self.save_current_canvas(format!(
                    "Change item background color to {}",
                    color_name.clone().unwrap_or("default".into())
                ));
                if let Some(waves) = self.waves.as_mut() {
                    if let Some(vidx) = vidx.or(waves.focused_item) {
                        waves.items_tree.get_visible(vidx).map(|node| {
                            waves
                                .displayed_items
                                .entry(node.item_ref)
                                .and_modify(|item| item.set_background_color(color_name.clone()))
                        });
                    }
                    if vidx.is_none() {
                        for node in waves.items_tree.iter_visible_selected() {
                            waves
                                .displayed_items
                                .entry(node.item_ref)
                                .and_modify(|item| item.set_background_color(color_name.clone()));
                        }
                    }
                };
            }
            Message::MoveCursorToTransition {
                next,
                variable,
                skip_zero,
            } => {
                if let Some(waves) = &mut self.waves {
                    // if no cursor is set, move it to
                    // start of visible area transition for next transition
                    // end of visible area for previous transition
                    if waves.cursor.is_none() && waves.focused_item.is_some() {
                        if let Some(vp) = waves.viewports.first() {
                            let num_timestamps = waves.num_timestamps().expect(
                                "No timestamps count, even though waveforms should be loaded",
                            );
                            waves.cursor = if next {
                                Some(vp.left_edge_time(&num_timestamps))
                            } else {
                                Some(vp.right_edge_time(&num_timestamps))
                            };
                        }
                    }
                    waves.set_cursor_at_transition(next, variable, skip_zero);
                    let moved = waves.go_to_cursor_if_not_in_view();
                    if moved {
                        self.invalidate_draw_commands();
                    }
                }
            }
            Message::MoveTransaction { next } => {
                let undo_msg = if next {
                    "Move to next transaction"
                } else {
                    "Move to previous transaction"
                };
                self.save_current_canvas(undo_msg.to_string());
                if let Some(waves) = &mut self.waves {
                    if let Some(inner) = waves.inner.as_transactions() {
                        let mut transactions = waves
                            .items_tree
                            .iter_visible()
                            .flat_map(|node| {
                                let item = &waves.displayed_items[&node.item_ref];
                                match item {
                                    DisplayedItem::Stream(s) => {
                                        let stream_ref = &s.transaction_stream_ref;
                                        let stream_id = stream_ref.stream_id;
                                        if let Some(gen_id) = stream_ref.gen_id {
                                            inner.get_transactions_from_generator(gen_id)
                                        } else {
                                            inner.get_transactions_from_stream(stream_id)
                                        }
                                    }
                                    _ => vec![],
                                }
                            })
                            .collect_vec();

                        transactions.sort();
                        let tx = if let Some(focused_tx) = &waves.focused_transaction.0 {
                            let next_id = transactions
                                .iter()
                                .enumerate()
                                .find(|(_, tx)| **tx == focused_tx.id)
                                .map(|(vec_idx, _)| {
                                    if next {
                                        if vec_idx + 1 < transactions.len() {
                                            vec_idx + 1
                                        } else {
                                            transactions.len() - 1
                                        }
                                    } else if vec_idx as i32 - 1 > 0 {
                                        vec_idx - 1
                                    } else {
                                        0
                                    }
                                })
                                .unwrap_or(if next { transactions.len() - 1 } else { 0 });
                            Some(TransactionRef {
                                id: *transactions.get(next_id).unwrap(),
                            })
                        } else if !transactions.is_empty() {
                            Some(TransactionRef {
                                id: *transactions.first().unwrap(),
                            })
                        } else {
                            None
                        };
                        waves.focused_transaction = (tx, waves.focused_transaction.1.clone());
                    }
                    self.invalidate_draw_commands();
                }
            }
            Message::ResetVariableFormat(displayed_field_ref) => {
                if let Some(DisplayedItem::Variable(displayed_variable)) = self
                    .waves
                    .as_mut()
                    .and_then(|waves| waves.displayed_items.get_mut(&displayed_field_ref.item))
                {
                    if displayed_field_ref.field.is_empty() {
                        displayed_variable.format = None;
                    } else {
                        displayed_variable
                            .field_formats
                            .retain(|ff| ff.field != displayed_field_ref.field);
                    }
                    self.invalidate_draw_commands();
                }
            }
            Message::CursorSet(time) => {
                if let Some(waves) = self.waves.as_mut() {
                    waves.cursor = Some(time);
                }
            }
            Message::LoadFile(filename, load_options) => {
                self.load_from_file(filename, load_options).ok();
            }
            Message::LoadWaveformFileFromUrl(url, load_options) => {
                self.load_wave_from_url(url, load_options);
            }
            Message::LoadFromData(data, load_options) => {
                self.load_from_data(data, load_options).ok();
            }
            #[cfg(feature = "python")]
            Message::LoadPythonTranslator(filename) => {
                try_log_error!(
                    self.sys.translators.load_python_translator(filename),
                    "Error loading Python translator",
                )
            }
            Message::LoadSpadeTranslator { top, state } => {
                #[cfg(feature = "spade")]
                {
                    let sender = self.sys.channels.msg_sender.clone();
                    perform_work(move || {
                        #[cfg(feature = "spade")]
                        SpadeTranslator::init(&top, &state, sender);
                    });
                };
                #[cfg(not(feature = "spade"))]
                {
                    info!(
                        "Surfer is not compiled with spade support, ignoring LoadSpadeTranslator"
                    );
                }
            }
            Message::SetupCxxrtl(kind) => self.connect_to_cxxrtl(kind, false),
            Message::SurferServerStatus(_start, server, status) => {
                self.server_status_to_progress(server, status);
            }
            Message::FileDropped(dropped_file) => {
                self.load_from_dropped(dropped_file)
                    .map_err(|e| error!("{e:#?}"))
                    .ok();
            }
            Message::WaveHeaderLoaded(start, source, load_options, header) => {
                // for files using the `wellen` backend, we load the header before parsing the body
                info!(
                    "Loaded the hierarchy and meta-data of {source} in {:?}",
                    start.elapsed()
                );
                match header {
                    HeaderResult::LocalFile(header) => {
                        // register waveform as loaded (but with no variable info yet!)
                        let shared_hierarchy = Arc::new(header.hierarchy);
                        let new_waves =
                            Box::new(WaveContainer::new_waveform(shared_hierarchy.clone()));
                        self.on_waves_loaded(
                            source.clone(),
                            convert_format(header.file_format),
                            new_waves,
                            load_options,
                        );
                        // start parsing of the body
                        self.load_wave_body(source, header.body, header.body_len, shared_hierarchy);
                    }
                    HeaderResult::LocalBytes(header) => {
                        // register waveform as loaded (but with no variable info yet!)
                        let shared_hierarchy = Arc::new(header.hierarchy);
                        let new_waves =
                            Box::new(WaveContainer::new_waveform(shared_hierarchy.clone()));
                        self.on_waves_loaded(
                            source.clone(),
                            convert_format(header.file_format),
                            new_waves,
                            load_options,
                        );
                        // start parsing of the body
                        self.load_wave_body(source, header.body, header.body_len, shared_hierarchy);
                    }
                    HeaderResult::Remote(hierarchy, file_format, server) => {
                        // register waveform as loaded (but with no variable info yet!)
                        let new_waves = Box::new(WaveContainer::new_remote_waveform(
                            server.clone(),
                            hierarchy.clone(),
                        ));
                        self.on_waves_loaded(
                            source.clone(),
                            convert_format(file_format),
                            new_waves,
                            load_options,
                        );
                        // body is already being parsed on the server, we need to request the time table though
                        Self::get_time_table_from_server(
                            self.sys.channels.msg_sender.clone(),
                            server,
                        );
                    }
                }
            }
            Message::WaveBodyLoaded(start, source, body) => {
                // for files using the `wellen` backend, parse the body in a second step
                info!("Loaded the body of {source} in {:?}", start.elapsed());
                self.sys.progress_tracker = None;
                let waves = self
                    .waves
                    .as_mut()
                    .expect("Waves should be loaded at this point!");
                // add source and time table
                let maybe_cmd = waves
                    .inner
                    .as_waves_mut()
                    .unwrap()
                    .wellen_add_body(body)
                    .unwrap_or_else(|err| {
                        error!("While getting commands to lazy-load signals: {err:?}");
                        None
                    });
                // Pre-load parameters
                let param_cmd = waves
                    .inner
                    .as_waves_mut()
                    .unwrap()
                    .load_parameters()
                    .unwrap_or_else(|err| {
                        error!("While getting commands to lazy-load parameters: {err:?}");
                        None
                    });

                if self.sys.wcp_server_load_outstanding {
                    self.sys.wcp_server_load_outstanding = false;
                    self.sys.channels.wcp_s2c_sender.as_ref().map(|ch| {
                        block_on(ch.send(WcpSCMessage::event(WcpEvent::waveforms_loaded)))
                    });
                }

                // update viewports, now that we have the time table
                waves.update_viewports();
                // make sure we redraw
                self.invalidate_draw_commands();
                // start loading parameters
                if let Some(cmd) = param_cmd {
                    self.load_variables(cmd);
                }
                // start loading variables
                if let Some(cmd) = maybe_cmd {
                    self.load_variables(cmd);
                }
            }
            Message::SignalsLoaded(start, res) => {
                info!("Loaded {} variables in {:?}", res.len(), start.elapsed());
                self.sys.progress_tracker = None;
                let waves = self
                    .waves
                    .as_mut()
                    .expect("Waves should be loaded at this point!");
                match waves.inner.as_waves_mut().unwrap().on_signals_loaded(res) {
                    Err(err) => error!("{err:?}"),
                    Ok(Some(cmd)) => self.load_variables(cmd),
                    _ => {}
                }
                // make sure we redraw since now more variable data is available
                self.invalidate_draw_commands();
            }
            Message::WavesLoaded(filename, format, new_waves, load_options) => {
                self.on_waves_loaded(filename, format, new_waves, load_options);
                // here, the body and thus the number of timestamps is already loaded!
                self.waves.as_mut().unwrap().update_viewports();
                self.sys.progress_tracker = None;
            }
            Message::TransactionStreamsLoaded(filename, format, new_ftr, loaded_options) => {
                self.on_transaction_streams_loaded(filename, format, new_ftr, loaded_options);
                self.waves.as_mut().unwrap().update_viewports();
            }
            Message::BlacklistTranslator(idx, translator) => {
                self.blacklisted_translators.insert((idx, translator));
            }
            Message::Error(e) => {
                error!("{e:?}");
                self.show_logs = true;
            }
            Message::TranslatorLoaded(t) => {
                info!("Translator {} loaded", t.name());
                self.sys.translators.add_or_replace(AnyTranslator::Full(t));
            }
            Message::ToggleSidePanel => {
                let new = match self.show_hierarchy {
                    Some(prev) => !prev,
                    None => !self.config.layout.show_hierarchy(),
                };
                self.show_hierarchy = Some(new);
            }
            Message::ToggleMenu => {
                let new = match self.show_menu {
                    Some(prev) => !prev,
                    None => !self.config.layout.show_menu(),
                };
                self.show_menu = Some(new);
            }
            Message::ToggleToolbar => {
                let new = match self.show_toolbar {
                    Some(prev) => !prev,
                    None => !self.config.layout.show_toolbar(),
                };
                self.show_toolbar = Some(new);
            }
            Message::ToggleEmptyScopes => {
                let new = match self.show_empty_scopes {
                    Some(prev) => !prev,
                    None => !self.config.layout.show_empty_scopes(),
                };
                self.show_empty_scopes = Some(new);
            }
            Message::ToggleParametersInScopes => {
                let new = match self.show_parameters_in_scopes {
                    Some(prev) => !prev,
                    None => !self.config.layout.show_parameters_in_scopes(),
                };
                self.show_parameters_in_scopes = Some(new);
            }
            Message::ToggleStatusbar => {
                let new = match self.show_statusbar {
                    Some(prev) => !prev,
                    None => !self.config.layout.show_statusbar(),
                };
                self.show_statusbar = Some(new);
            }
            Message::ToggleTickLines => {
                let new = match self.show_ticks {
                    Some(prev) => !prev,
                    None => !self.config.layout.show_ticks(),
                };
                self.show_ticks = Some(new);
            }
            Message::ToggleVariableTooltip => {
                let new = match self.show_tooltip {
                    Some(prev) => !prev,
                    None => !self.config.layout.show_tooltip(),
                };
                self.show_tooltip = Some(new);
            }
            Message::ToggleScopeTooltip => {
                let new = match self.show_scope_tooltip {
                    Some(prev) => !prev,
                    None => !self.config.layout.show_scope_tooltip(),
                };
                self.show_scope_tooltip = Some(new);
            }
            Message::ToggleOverview => {
                let new = match self.show_overview {
                    Some(prev) => !prev,
                    None => !self.config.layout.show_overview(),
                };
                self.show_overview = Some(new);
            }
            Message::ToggleDirection => {
                let new = match self.show_variable_direction {
                    Some(prev) => !prev,
                    None => !self.config.layout.show_variable_direction(),
                };
                self.show_variable_direction = Some(new);
            }
            Message::ToggleIndices => {
                let new = match self.show_variable_indices {
                    Some(prev) => !prev,
                    None => !self.config.layout.show_variable_indices(),
                };
                self.show_variable_indices = Some(new);
                if let Some(waves) = self.waves.as_mut() {
                    waves.display_variable_indices = new;
                    waves.compute_variable_display_names();
                }
            }
            Message::ShowCommandPrompt(text) => {
                if let Some(init_text) = text {
                    self.sys.command_prompt.new_cursor_pos = Some(init_text.len());
                    *self.sys.command_prompt_text.borrow_mut() = init_text;
                    self.sys.command_prompt.visible = true;
                } else {
                    *self.sys.command_prompt_text.borrow_mut() = "".to_string();
                    self.sys.command_prompt.suggestions = vec![];
                    self.sys.command_prompt.selected =
                        self.sys.command_prompt.previous_commands.len();
                    self.sys.command_prompt.visible = false;
                }
            }
            Message::FileDownloaded(url, bytes, load_options) => {
                self.load_from_bytes(WaveSource::Url(url), bytes.to_vec(), load_options)
            }
            Message::SetConfigFromString(s) => {
                // FIXME think about a structured way to collect errors
                if let Ok(config) =
                    SurferConfig::new_from_toml(&s).with_context(|| "Failed to load config file")
                {
                    self.config = config;
                    if let Some(ctx) = &self.sys.context.as_ref() {
                        ctx.set_visuals(self.get_visuals())
                    }
                }
            }
            Message::ReloadConfig => {
                // FIXME think about a structured way to collect errors
                if let Ok(config) =
                    SurferConfig::new(false).with_context(|| "Failed to load config file")
                {
                    self.sys.translators = all_translators();
                    self.config = config;
                    if let Some(ctx) = &self.sys.context.as_ref() {
                        ctx.set_visuals(self.get_visuals());
                    }
                }
            }
            Message::ReloadWaveform(keep_unavailable) => {
                let Some(waves) = &self.waves else { return };
                match &waves.source {
                    WaveSource::File(filename) => {
                        self.load_from_file(
                            filename.clone(),
                            LoadOptions {
                                keep_variables: true,
                                keep_unavailable,
                            },
                        )
                        .ok();
                    }
                    WaveSource::Data => {}       // can't reload
                    WaveSource::Cxxrtl(..) => {} // can't reload
                    WaveSource::DragAndDrop(filename) => {
                        filename.clone().and_then(|filename| {
                            self.load_from_file(
                                filename,
                                LoadOptions {
                                    keep_variables: true,
                                    keep_unavailable,
                                },
                            )
                            .ok()
                        });
                    }
                    WaveSource::Url(url) => {
                        self.load_wave_from_url(
                            url.clone(),
                            LoadOptions {
                                keep_variables: true,
                                keep_unavailable,
                            },
                        );
                    }
                };

                for translator in self.sys.translators.all_translators() {
                    translator.reload(self.sys.channels.msg_sender.clone());
                }
            }
            Message::SuggestReloadWaveform => match self.config.autoreload_files {
                Some(true) => {
                    self.update(Message::ReloadWaveform(true));
                }
                Some(false) => {}
                None => self.show_reload_suggestion = Some(ReloadWaveformDialog::default()),
            },
            Message::CloseReloadWaveformDialog {
                reload_file,
                do_not_show_again,
            } => {
                if do_not_show_again {
                    // FIXME: This is currently for one session only, but could be persisted in
                    // some setting.
                    self.config.autoreload_files = Some(reload_file);
                }
                self.show_reload_suggestion = None;
                if reload_file {
                    self.update(Message::ReloadWaveform(true));
                }
            }
            Message::UpdateReloadWaveformDialog(dialog) => {
                self.show_reload_suggestion = Some(dialog);
            }
            Message::RemovePlaceholders => {
                if let Some(waves) = self.waves.as_mut() {
                    waves.remove_placeholders();
                }
            }
            Message::SetClockHighlightType(new_type) => {
                self.config.default_clock_highlight_type = new_type;
            }
            Message::AddMarker { time, name } => {
                if let Some(name) = &name {
                    self.save_current_canvas(format!("Add marker {name} at {time}"));
                } else {
                    self.save_current_canvas(format!("Add marker at {time}"));
                }
                if let Some(waves) = self.waves.as_mut() {
                    waves.add_marker(&time, name);
                }
            }
            Message::SetMarker { id, time } => {
                self.save_current_canvas(format!("Set marker {id} to {time}"));
                if let Some(waves) = self.waves.as_mut() {
                    waves.set_marker_position(id, &time);
                };
            }
            Message::RemoveMarker(id) => {
                if let Some(waves) = self.waves.as_mut() {
                    waves.remove_marker(id);
                }
            }
            Message::MoveMarkerToCursor(idx) => {
                self.save_current_canvas("Move marker".into());
                if let Some(waves) = self.waves.as_mut() {
                    waves.move_marker_to_cursor(idx);
                };
            }
            Message::GoToCursorIfNotInView => {
                if let Some(waves) = self.waves.as_mut() {
                    if waves.go_to_cursor_if_not_in_view() {
                        self.invalidate_draw_commands();
                    }
                }
            }
            Message::GoToMarkerPosition(idx, viewport_idx) => {
                if let Some(waves) = self.waves.as_mut() {
                    if let Some(cursor) = waves.markers.get(&idx) {
                        let num_timestamps = waves
                            .num_timestamps()
                            .expect("No timestamps count, even though waveforms should be loaded");
                        waves.viewports[viewport_idx].go_to_time(cursor, &num_timestamps);
                        self.invalidate_draw_commands();
                    }
                };
            }
            Message::ChangeVariableNameType(vidx, name_type) => {
                let Some(waves) = self.waves.as_mut() else {
                    return;
                };
                // checks if vidx is Some then use that, else try focused variable
                if let Some(vidx) = vidx.or(waves.focused_item) {
                    let Some(item_ref) =
                        waves.items_tree.get_visible(vidx).map(|node| node.item_ref)
                    else {
                        return;
                    };
                    let mut recompute_names = false;
                    waves.displayed_items.entry(item_ref).and_modify(|item| {
                        if let DisplayedItem::Variable(variable) = item {
                            variable.display_name_type = name_type;
                            recompute_names = true;
                        }
                    });
                    if recompute_names {
                        waves.compute_variable_display_names();
                    }
                }
            }
            Message::ForceVariableNameTypes(name_type) => {
                if let Some(waves) = self.waves.as_mut() {
                    waves.force_variable_name_type(name_type);
                };
            }
            Message::CommandPromptClear => {
                *self.sys.command_prompt_text.borrow_mut() = String::new();
                self.sys.command_prompt.suggestions = vec![];
                // self.sys.command_prompt.selected = self.sys.command_prompt.previous_commands.len();
                self.sys.command_prompt.selected =
                    if self.sys.command_prompt_text.borrow().is_empty() {
                        self.sys.command_prompt.previous_commands.len().clamp(0, 3)
                    } else {
                        0
                    };
            }
            Message::CommandPromptUpdate { suggestions } => {
                self.sys.command_prompt.suggestions = suggestions;
                self.sys.command_prompt.selected =
                    if self.sys.command_prompt_text.borrow().is_empty() {
                        self.sys.command_prompt.previous_commands.len().clamp(0, 3)
                    } else {
                        0
                    };
                self.sys.command_prompt.new_selection =
                    Some(if self.sys.command_prompt_text.borrow().is_empty() {
                        self.sys.command_prompt.previous_commands.len().clamp(0, 3)
                    } else {
                        0
                    });
            }
            Message::CommandPromptPushPrevious(cmd) => {
                let len = cmd.len();
                self.sys
                    .command_prompt
                    .previous_commands
                    .insert(0, (cmd, vec![false; len]));
            }
            Message::OpenFileDialog(mode) => {
                self.open_file_dialog(mode);
            }
            #[cfg(feature = "python")]
            Message::OpenPythonPluginDialog => {
                self.open_python_file_dialog();
            }
            #[cfg(feature = "python")]
            Message::ReloadPythonPlugin => {
                try_log_error!(
                    self.sys.translators.reload_python_translator(),
                    "Error reloading Python translator"
                );
                self.invalidate_draw_commands();
            }
            Message::SaveStateFile(path) => self.save_state_file(path),
            Message::LoadStateFile(path) => self.load_state_file(path),
            Message::LoadState(state, path) => self.load_state(state, path),
            Message::SetStateFile(path) => {
                // since in wasm we can't support "save", only "save as" - never set the `state_file`
                #[cfg(not(target_arch = "wasm32"))]
                {
                    self.state_file = Some(path);
                }
                #[cfg(target_arch = "wasm32")]
                {
                    error!("Failed to load {path:?}. Loading state files is unsupported on wasm")
                }
            }
            Message::SetAboutVisible(s) => self.show_about = s,
            Message::SetKeyHelpVisible(s) => self.show_keys = s,
            Message::SetGestureHelpVisible(s) => self.show_gestures = s,
            Message::SetUrlEntryVisible(s) => self.show_url_entry = s,
            Message::SetLicenseVisible(s) => self.show_license = s,
            Message::SetQuickStartVisible(s) => self.show_quick_start = s,
            Message::SetRenameItemVisible(_) => self.rename_target = None,
            Message::SetPerformanceVisible(s) => {
                if !s {
                    self.sys.continuous_redraw = false;
                }
                self.show_performance = s;
            }
            Message::SetContinuousRedraw(s) => self.sys.continuous_redraw = s,
            Message::SetDragStart(pos) => self.sys.gesture_start_location = pos,
            Message::SetFilterFocused(s) => self.variable_name_filter_focused = s,
            Message::SetVariableNameFilterType(variable_name_filter_type) => {
                self.variable_name_filter_type = variable_name_filter_type;
            }
            Message::SetVariableNameFilterCaseInsensitive(s) => {
                self.variable_name_filter_case_insensitive = s;
            }
            Message::SetUIZoomFactor(scale) => {
                if let Some(ctx) = &mut self.sys.context.as_ref() {
                    ctx.set_zoom_factor(scale);
                }
                self.ui_zoom_factor = Some(scale);
            }
            Message::SelectPrevCommand => {
                self.sys.command_prompt.new_selection = self
                    .sys
                    .command_prompt
                    .new_selection
                    .or(Some(self.sys.command_prompt.selected))
                    .map(|idx| idx.saturating_sub(1).max(0));
            }
            Message::SelectNextCommand => {
                self.sys.command_prompt.new_selection = self
                    .sys
                    .command_prompt
                    .new_selection
                    .or(Some(self.sys.command_prompt.selected))
                    .map(|idx| {
                        idx.saturating_add(1)
                            .min(self.sys.command_prompt.suggestions.len().saturating_sub(1))
                    });
            }
            Message::SetHierarchyStyle(style) => self.config.layout.hierarchy_style = style,
            Message::SetArrowKeyBindings(bindings) => {
                self.config.behavior.arrow_key_bindings = bindings;
            }
            Message::InvalidateDrawCommands => self.invalidate_draw_commands(),
            Message::UnpauseSimulation => {
                if let Some(waves) = &self.waves {
                    waves.inner.as_waves().unwrap().unpause_simulation();
                }
            }
            Message::PauseSimulation => {
                if let Some(waves) = &self.waves {
                    waves.inner.as_waves().unwrap().pause_simulation();
                }
            }
            Message::Batch(messages) => {
                for message in messages {
                    self.update(message);
                }
            }
            Message::AddDraggedVariables(variables) => {
                if self.waves.is_some() {
                    self.waves.as_mut().unwrap().focused_item = None;
                    let waves = self.waves.as_mut().unwrap();
                    if let (Some(cmd), _) =
                        waves.add_variables(&self.sys.translators, variables, self.drag_target_idx)
                    {
                        self.load_variables(cmd);
                    }

                    self.invalidate_draw_commands();
                }
                self.drag_source_idx = None;
                self.drag_target_idx = None;
            }
            Message::VariableDragStarted(vidx) => {
                self.drag_started = true;
                self.drag_source_idx = Some(vidx);
                self.drag_target_idx = None;
            }
            Message::VariableDragTargetChanged(position) => {
                self.drag_target_idx = Some(position);
            }
            Message::VariableDragFinished => {
                self.drag_started = false;

                // reordering
                if let (Some(source_vidx), Some(target_position)) =
                    (self.drag_source_idx, self.drag_target_idx)
                {
                    self.save_current_canvas("Drag item".to_string());
                    self.invalidate_draw_commands();
                    let Some(waves) = self.waves.as_mut() else {
                        return;
                    };

                    let focused_index = waves
                        .focused_item
                        .and_then(|vidx| waves.items_tree.to_displayed(vidx));
                    let focused_item_ref = focused_index
                        .and_then(|idx| waves.items_tree.get(idx))
                        .map(|node| node.item_ref);

                    let mut to_move = waves
                        .items_tree
                        .iter_visible_extra()
                        .filter_map(
                            |(node, idx, _, _)| if node.selected { Some(idx) } else { None },
                        )
                        .collect::<Vec<_>>();
                    if let Some(idx) = focused_index {
                        to_move.push(idx)
                    };
                    if let Some(vidx) = waves.items_tree.to_displayed(source_vidx) {
                        to_move.push(vidx)
                    };

                    let _ = waves.items_tree.move_items(to_move, target_position);

                    waves.focused_item = focused_item_ref
                        .and_then(|item_ref| {
                            waves
                                .items_tree
                                .iter_visible()
                                .position(|node| node.item_ref == item_ref)
                        })
                        .map(VisibleItemIndex);
                }
                self.drag_source_idx = None;
                self.drag_target_idx = None;
            }
            Message::VariableValueToClipbord(vidx) => {
                if let Some(waves) = &self.waves {
                    if let Some(vidx) = vidx.or(waves.focused_item) {
                        if let Some(item_ref) =
                            waves.items_tree.get_visible(vidx).map(|node| node.item_ref)
                        {
                            let Some(DisplayedItem::Variable(_displayed_variable)) =
                                waves.displayed_items.get(&item_ref)
                            else {
                                return;
                            };

                            let field_ref = item_ref.into();
                            let variable_value = self.get_variable_value(
                                waves,
                                &field_ref,
                                &waves.cursor.as_ref().and_then(num::BigInt::to_biguint),
                            );
                            if let Some(variable_value) = variable_value {
                                if let Some(ctx) = &self.sys.context {
                                    ctx.output_mut(|o| o.copied_text = variable_value);
                                }
                            }
                        }
                    }
                }
            }
            Message::SetViewportStrategy(s) => {
                if let Some(waves) = &mut self.waves {
                    for vp in &mut waves.viewports {
                        vp.move_strategy = s
                    }
                }
            }
            Message::Undo(count) => {
                if let Some(waves) = &mut self.waves {
                    for _ in 0..count {
                        if let Some(prev_state) = self.sys.undo_stack.pop() {
                            self.sys
                                .redo_stack
                                .push(State::current_canvas_state(waves, prev_state.message));
                            waves.focused_item = prev_state.focused_item;
                            waves.focused_transaction = prev_state.focused_transaction;
                            waves.items_tree = prev_state.items_tree;
                            waves.displayed_items = prev_state.displayed_items;
                            waves.markers = prev_state.markers;
                        } else {
                            break;
                        }
                    }
                    self.invalidate_draw_commands();
                }
            }
            Message::Redo(count) => {
                if let Some(waves) = &mut self.waves {
                    for _ in 0..count {
                        if let Some(prev_state) = self.sys.redo_stack.pop() {
                            self.sys
                                .undo_stack
                                .push(State::current_canvas_state(waves, prev_state.message));
                            waves.focused_item = prev_state.focused_item;
                            waves.focused_transaction = prev_state.focused_transaction;
                            waves.items_tree = prev_state.items_tree;
                            waves.displayed_items = prev_state.displayed_items;
                            waves.markers = prev_state.markers;
                        } else {
                            break;
                        }
                    }
                    self.invalidate_draw_commands();
                }
            }
            Message::DumpTree => {
                let Some(waves) = self.waves.as_mut() else {
                    return;
                };

                dump_tree(waves);
            }
            Message::GroupNew {
                name,
                target_position,
                items,
            } => {
                self.save_current_canvas(format!(
                    "Create group {}",
                    name.clone().unwrap_or("".to_owned())
                ));
                self.invalidate_draw_commands();
                let Some(waves) = self.waves.as_mut() else {
                    return;
                };

                let passed_or_focused = target_position.or_else(|| waves.focused_insert_position());
                let final_target = passed_or_focused.unwrap_or_else(|| waves.end_insert_position());

                let mut item_refs = items.unwrap_or_else(|| {
                    waves
                        .items_tree
                        .iter_visible_selected()
                        .map(|node| node.item_ref)
                        .collect::<Vec<_>>()
                });

                // if we are using the focus as the insert anchor, then move that as well
                let item_refs = if target_position.is_none() && passed_or_focused.is_some() {
                    info!("moving focus item");
                    let focus_index = waves
                        .items_tree
                        .to_displayed(waves.focused_item.expect("Inconsistent state"))
                        .expect("Inconsistent state");
                    item_refs.push(
                        waves
                            .items_tree
                            .get(focus_index)
                            .expect("Inconsistent state")
                            .item_ref,
                    );
                    item_refs
                } else {
                    item_refs
                };

                dump_tree(waves);
                info!("final_target: {final_target:?}");
                info!("moving: {item_refs:?}");
                let group_ref =
                    waves.add_group(name.unwrap_or("Group".to_owned()), Some(final_target));
                let insert_idx = final_target.before;
                info!("insert_idx: {insert_idx:?}");

                let item_idxs = waves
                    .items_tree
                    .iter()
                    .enumerate()
                    .filter_map(|(idx, node)| {
                        item_refs
                            .contains(&node.item_ref)
                            .then_some(crate::displayed_item_tree::ItemIndex(idx))
                    })
                    .collect::<Vec<_>>();
                info!("post indices: {item_idxs:?}");

                if let Err(e) = waves.items_tree.move_items(
                    item_idxs,
                    crate::displayed_item_tree::TargetPosition {
                        before: final_target.before + 1,
                        level: final_target.level.saturating_add(1),
                    },
                ) {
                    dump_tree(waves);
                    waves.remove_displayed_item(group_ref);
                    error!("failed to move items into group: {e:?}")
                }
                waves.items_tree.xselect_all(false);
            }
            Message::GroupDissolve(item_ref) => {
                self.save_current_canvas("Dissolve group".to_owned());
                self.invalidate_draw_commands();
                let Some(waves) = self.waves.as_mut() else {
                    return;
                };

                let Some(item_index) = waves.index_for_ref_or_focus(item_ref) else {
                    return;
                };

                waves.items_tree.remove_dissolve(item_index);
            }
            Message::GroupFold(item_ref)
            | Message::GroupUnfold(item_ref)
            | Message::GroupFoldRecursive(item_ref)
            | Message::GroupUnfoldRecursive(item_ref) => {
                let unfold = matches!(
                    message,
                    Message::GroupUnfold(..) | Message::GroupUnfoldRecursive(..)
                );
                let recursive = matches!(
                    message,
                    Message::GroupFoldRecursive(..) | Message::GroupUnfoldRecursive(..)
                );

                let undo_msg = if unfold {
                    "Unfold group".to_owned()
                } else {
                    "Fold group".to_owned()
                } + &(if recursive {
                    " recursive".to_owned()
                } else {
                    "".to_owned()
                });
                // TODO add group name? would have to break the pattern that we insert an
                // undo message even if no waves are available
                self.save_current_canvas(undo_msg);
                self.invalidate_draw_commands();

                let Some(waves) = self.waves.as_mut() else {
                    return;
                };

                let Some(item) = waves.index_for_ref_or_focus(item_ref) else {
                    return;
                };

                if let Some(focused_item) = waves.focused_item {
                    let (_, focused_index, _, _) = waves
                        .items_tree
                        .get_visible_extra(focused_item)
                        .expect("Inconsistent state");
                    if waves.items_tree.subtree_contains(item, focused_index) {
                        waves.focused_item = None;
                    }
                }
                if recursive {
                    waves.items_tree.xfold_subtree(item, unfold);
                } else {
                    waves.items_tree.xfold(item, unfold);
                }
            }
            Message::GroupFoldAll | Message::GroupUnfoldAll => {
                let unfold = matches!(message, Message::GroupUnfoldAll);
                let undo_msg = if unfold {
                    "Fold all groups".to_owned()
                } else {
                    "Unfold all groups".to_owned()
                };
                self.save_current_canvas(undo_msg);
                self.invalidate_draw_commands();

                let Some(waves) = self.waves.as_mut() else {
                    return;
                };
                // remove focus if focused item is folded away -> prevent future waveform
                // adds being invisibly inserted
                if let Some(focused_item) = waves.focused_item {
                    let focused_level = waves
                        .items_tree
                        .get_visible(focused_item)
                        .expect("Inconsistent state")
                        .level;
                    if !unfold && focused_level > 0 {
                        waves.focused_item = None;
                    }
                }
                waves.items_tree.xfold_all(unfold);
            }
            #[cfg(target_arch = "wasm32")]
            Message::StartWcpServer { .. } => {
                error!("Wcp is not supported on wasm")
            }
            #[cfg(target_arch = "wasm32")]
            Message::StopWcpServer => {
                error!("Wcp is not supported on wasm")
            }
            #[cfg(not(target_arch = "wasm32"))]
            Message::StartWcpServer { address, initiate } => {
                self.start_wcp_server(address, initiate);
            }
            #[cfg(not(target_arch = "wasm32"))]
            Message::StopWcpServer => {
                self.stop_wcp_server();
            }
            Message::SetupChannelWCP => {
                use futures::executor::block_on;
                self.sys.channels.wcp_c2s_receiver = block_on(WCP_CS_HANDLER.rx.write()).take();
                if self.sys.channels.wcp_c2s_receiver.is_none() {
                    error!("Failed to claim wasm tx, was SetupWasmWCP executed twice?");
                }
                self.sys.channels.wcp_s2c_sender = Some(WCP_SC_HANDLER.tx.clone());
            }
            Message::Exit | Message::ToggleFullscreen => {} // Handled in eframe::update
            Message::AddViewport => {
                if let Some(waves) = &mut self.waves {
                    let viewport = Viewport::new();
                    waves.viewports.push(viewport);
                    self.sys.draw_data.borrow_mut().push(None);
                }
            }
            Message::RemoveViewport => {
                if let Some(waves) = &mut self.waves {
                    if waves.viewports.len() > 1 {
                        waves.viewports.pop();
                        self.sys.draw_data.borrow_mut().pop();
                    }
                }
            }
            Message::SelectTheme(theme_name) => {
                if let Ok(theme) =
                    SurferTheme::new(theme_name).with_context(|| "Failed to set theme")
                {
                    self.config.theme = theme;
                    if let Some(ctx) = &self.sys.context.as_ref() {
                        ctx.set_visuals(self.get_visuals());
                    }
                }
            }
            Message::AsyncDone(_) => (),
            Message::AddGraphic(id, g) => {
                if let Some(waves) = &mut self.waves {
                    waves.graphics.insert(id, g);
                }
            }
            Message::RemoveGraphic(id) => {
                if let Some(waves) = &mut self.waves {
                    waves.graphics.retain(|k, _| k != &id)
                }
            }
            Message::ExpandDrawnItem { item, levels } => {
                self.sys.items_to_expand.borrow_mut().push((item, levels))
            }
            Message::AddCharToPrompt(c) => *self.sys.char_to_add_to_prompt.borrow_mut() = Some(c),
        }
    }
}

pub fn dump_tree(waves: &WaveData) {
    let mut result = String::new();
    for (idx, node) in waves.items_tree.iter().enumerate() {
        for _ in 0..node.level.saturating_sub(1) {
            result.push(' ');
        }

        if node.level > 0 {
            match waves.items_tree.items.get(idx + 1) {
                Some(next) if next.level < node.level => result.push_str("╰╴"),
                _ => result.push_str("├╴"),
            }
        }

        result.push_str(
            &waves
                .displayed_items
                .get(&node.item_ref)
                .map(|item| item.name())
                .unwrap_or("?".to_owned()),
        );
        result.push_str(&format!("   ({:?})", node.item_ref));
        if node.selected {
            result.push_str(" !SEL! ")
        }
        result.push('\n');
    }
    info!("tree: \n{}", &result);
}

pub struct StateWrapper(Arc<RwLock<State>>);
impl App for StateWrapper {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        App::update(&mut *self.0.write().unwrap(), ctx, frame)
    }
}
