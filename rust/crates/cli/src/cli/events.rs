use clap::{Args, Subcommand};

#[derive(Debug, Subcommand)]
/// Query and administer Events.
pub enum EventsCommand {
    Query(PageArgs),
    Inspect(EventInspectArgs),
    #[command(subcommand)]
    Consumers(ConsumersCommand),
    #[command(subcommand)]
    DeadLetters(DeadLettersCommand),
}

#[derive(Debug, Args, Clone)]
pub struct PageArgs {
    #[arg(long)]
    pub cursor: Option<String>,
    #[arg(long)]
    pub limit: Option<u32>,
    #[arg(long)]
    pub all: bool,
}

#[derive(Debug, Args)]
pub struct EventInspectArgs {
    #[arg(
        long,
        conflicts_with = "stream_sequence",
        required_unless_present = "stream_sequence"
    )]
    pub event_id: Option<String>,
    #[arg(
        long,
        conflicts_with = "event_id",
        required_unless_present = "event_id"
    )]
    pub stream_sequence: Option<i64>,
}

#[derive(Debug, Subcommand)]
pub enum ConsumersCommand {
    Query(ConsumerQueryArgs),
    Inspect(ResourceIdArgs),
}

#[derive(Debug, Args)]
pub struct ConsumerQueryArgs {
    #[command(flatten)]
    pub page: PageArgs,
    #[arg(long)]
    pub resource_id: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum DeadLettersCommand {
    Query(DeadLetterQueryArgs),
    Inspect(DeadLetterRefArgs),
    Replay(DeadLetterMutationArgs),
    Dismiss(DeadLetterMutationArgs),
}

#[derive(Debug, Args)]
pub struct DeadLetterQueryArgs {
    #[command(flatten)]
    pub page: PageArgs,
    #[arg(long)]
    pub resource_id: Option<String>,
}

#[derive(Debug, Args)]
pub struct ResourceIdArgs {
    pub resource_id: String,
}

#[derive(Debug, Args)]
pub struct DeadLetterRefArgs {
    pub resource_id: String,
    pub dead_letter_id: String,
}

#[derive(Debug, Args)]
pub struct DeadLetterMutationArgs {
    pub resource_id: String,
    pub dead_letter_id: String,
    #[arg(long)]
    pub expected_revision: u64,
    /// Stable command ID; reuse it when retrying the same mutation.
    #[arg(long)]
    pub request_id: String,
}
