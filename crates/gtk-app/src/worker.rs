//! Bounded, serialized D-Bus work. GTK never waits for audio writes.
use crate::{Bus, Snapshot};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::time::Duration;

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    Set(&'static str, f64),
    Profile(String),
    Output(String),
}

pub struct Event {
    /// Includes superseded slider values that were coalesced before writing.
    pub completed: usize,
    pub snapshot: Option<Snapshot>,
    pub error: Option<String>,
}

/// Coalesce only consecutive slider writes. Profile/output operations are
/// ordering barriers, so a drag cannot migrate across a profile change.
fn coalesce(commands: Vec<Command>) -> Vec<(Vec<Command>, usize)> {
    let mut jobs = Vec::new();
    let mut sets = Vec::new();
    let mut count = 0;
    for command in commands {
        if let Command::Set(name, _) = &command {
            count += 1;
            if let Some(index) = sets
                .iter()
                .position(|item| matches!(item, Command::Set(other, _) if other == name))
            {
                sets[index] = command;
            } else {
                sets.push(command);
            }
        } else {
            if count != 0 {
                jobs.push((std::mem::take(&mut sets), count));
                count = 0;
            }
            jobs.push((vec![command], 1));
        }
    }
    if count != 0 {
        jobs.push((sets, count));
    }
    jobs
}

pub fn start(bus: Bus) -> (SyncSender<Command>, Receiver<Event>) {
    let (sender, commands) = mpsc::sync_channel(64);
    let (events, receiver) = mpsc::sync_channel(8);
    std::thread::spawn(move || loop {
        let first = match commands.recv_timeout(Duration::from_secs(2)) {
            Ok(command) => command,
            Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {
                let snapshot = bus.snapshot();
                let event = match snapshot {
                    Ok(snapshot) => Event {
                        completed: 0,
                        snapshot: Some(snapshot),
                        error: None,
                    },
                    Err(error) => Event {
                        completed: 0,
                        snapshot: None,
                        error: Some(error.to_string()),
                    },
                };
                if events.send(event).is_err() {
                    break;
                }
                continue;
            }
        };
        let mut batch = vec![first];
        batch.extend(commands.try_iter().take(63));
        for (job, completed) in coalesce(batch) {
            let mut error = None;
            for command in job {
                let result = match command {
                    Command::Set(name, value) => bus.set(name, value),
                    Command::Profile(name) => bus.apply_profile(&name),
                    Command::Output(name) => bus.set_output_device(&name),
                };
                if let Err(failure) = result {
                    error.get_or_insert_with(|| failure.to_string());
                }
            }
            let snapshot = match bus.snapshot() {
                Ok(snapshot) => Some(snapshot),
                Err(failure) => {
                    error.get_or_insert_with(|| failure.to_string());
                    None
                }
            };
            if events
                .send(Event {
                    completed,
                    snapshot,
                    error,
                })
                .is_err()
            {
                return;
            }
        }
    });
    (sender, receiver)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slider_burst_keeps_latest_values_and_accounts_for_every_request() {
        let jobs = coalesce(vec![
            Command::Set("bass", 0.1),
            Command::Set("clarity", 0.2),
            Command::Set("bass", 0.9),
        ]);
        assert_eq!(
            jobs,
            vec![(
                vec![Command::Set("bass", 0.9), Command::Set("clarity", 0.2)],
                3
            )]
        );
    }

    #[test]
    fn profile_and_output_are_ordering_barriers() {
        let jobs = coalesce(vec![
            Command::Set("bass", 0.1),
            Command::Set("bass", 0.2),
            Command::Profile("Cinema".into()),
            Command::Set("bass", 0.8),
            Command::Output("speakers".into()),
            Command::Set("bass", 0.9),
        ]);
        assert_eq!(jobs.len(), 5);
        assert_eq!(jobs[0], (vec![Command::Set("bass", 0.2)], 2));
        assert_eq!(jobs[1], (vec![Command::Profile("Cinema".into())], 1));
        assert_eq!(jobs[2], (vec![Command::Set("bass", 0.8)], 1));
        assert_eq!(jobs[3], (vec![Command::Output("speakers".into())], 1));
        assert_eq!(jobs[4], (vec![Command::Set("bass", 0.9)], 1));
    }
}
