use clap::Subcommand;

#[derive(Debug, clap::Args)]
#[command(args_conflicts_with_subcommands = true)]
pub(crate) struct AgentsArgs {
    #[command(subcommand)]
    pub(crate) command: Option<AgentsCommand>,

    #[command(flatten)]
    pub(crate) legacy: AgentsLegacyArgs,
}

#[derive(Debug, clap::Args)]
pub(crate) struct AgentsLegacyArgs {
    pub(crate) project_id: Option<String>,
    #[arg(long)]
    pub(crate) profile: Option<String>,
    #[arg(long)]
    pub(crate) json: bool,
}

#[derive(Debug, Subcommand)]
pub(crate) enum AgentsCommand {
    List {
        project_id: String,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        json: bool,
    },
    Capabilities {
        project_id: String,
        agent_id: String,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        json: bool,
    },
    Group {
        project_id: String,
        group_name: String,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        json: bool,
    },
}
