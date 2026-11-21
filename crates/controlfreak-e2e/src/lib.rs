mod desktops;
pub mod events;
pub mod github;
pub mod host;
pub mod mcp;
pub mod pr_report;
pub mod report;
pub mod runner;
pub mod scene;
pub mod suites;
pub mod workflow;

pub use workflow::{Capability, Ctx, Failure, Suite, Task};
