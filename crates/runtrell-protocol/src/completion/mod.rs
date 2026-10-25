// SPDX-License-Identifier: Apache-2.0
//! Model-independent messages, requests, replies and adapter extension data.
pub mod history;
pub mod message;
pub mod options;
pub mod provider_options;
pub mod request;
pub use history::{Accepts, LaterSystem, Media, Pairing, Place, Replay, ReplayTarget, adapt};
pub use message::{AssistantContent, AssistantMessage, Message, MessageError};
pub use options::{
    CacheRetention, Effort, GenerationOptions, OnUnsupported, Reasoning, ServiceTier,
    UnsupportedOption, Verbosity,
};
pub use provider_options::{
    ExtensionOptions, OptionsError, ProviderExtension, ProviderOptions, ReplyExtras, SHARED,
};
pub use request::*;

pub mod handle;
pub use handle::ModelRef;
