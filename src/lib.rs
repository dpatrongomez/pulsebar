//! PulseBar — app-centric activity monitor + process inspector for macOS developers.
//!
//! Privacy: everything is processed on this Mac. The core makes **no network calls**; the only
//! two permitted ones (update check, licence validation) are isolated in `net_policy` and are
//! disabled unless the user opts in.

#![allow(clippy::type_complexity, clippy::needless_range_loop)]

pub mod actions;
pub mod agents;
pub mod alerts;
pub mod cli;
pub mod collectors;
pub mod daemon;
pub mod docker;
pub mod grouping;
pub mod history;
pub mod i18n;
pub mod inspector;
pub mod limits;
pub mod live;
pub mod login_item;
pub mod mcp;
pub mod model;
pub mod net_policy;
pub mod output;
pub mod paths;
pub mod projects;
pub mod sampler;
pub mod search;
pub mod settings;
pub mod share_card;
pub mod sorting;
pub mod sys;
pub mod units;
pub mod views;

#[cfg(feature = "gui")]
pub mod gui;
