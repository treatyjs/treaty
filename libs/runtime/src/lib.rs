//! Treaty JS-engine runtime.
//!
//! Hosts the JavaScript execution layer shared by macro/RSC (pre)render execution, TS
//! server-function execution, and the serverless server-side runtime. Engine choice: Nova
//! (pure-Rust, preferred) with Boa as the fallback — selected behind an engine abstraction.
//!
//! (rusty_v8 was ruled out on this Windows host: its build script needs the symbolic-link
//! privilege / Developer Mode, which is unavailable here.)

/// Placeholder pending the Nova/Boa engine integration.
pub fn placeholder() -> u32 {
    3
}
