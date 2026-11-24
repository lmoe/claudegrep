use crate::test_support::lines::Transcript;
use crate::transcript::{self, Session};

pub fn parse(id: &str, transcript: &Transcript) -> Session {
    transcript::parse(id, &transcript.build()).expect("the transcript has an exchange")
}

pub fn parse_with_agents(id: &str, main: &Transcript, agents: &[(&Transcript, Option<&str>)]) -> Session {
    let reports = agents.iter().filter_map(|(agent, meta)| transcript::agent_report(&agent.build(), *meta)).collect();
    transcript::with_reports(parse(id, main), reports)
}
