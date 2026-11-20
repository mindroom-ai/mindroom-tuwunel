//! Test only: a pause an SSO sign-in makes just before its link is stored,
//! still holding the account and, for a claim, the appservices it read.

use std::sync::Mutex;

use ruma::{OwnedUserId, UserId};
use tokio::sync::oneshot::{Receiver, Sender, channel};

/// The sign-in to pause: its account, the signal that it paused, and its
/// resumption.
type Armed = (OwnedUserId, Sender<()>, Receiver<()>);

static ARMED: Mutex<Option<Armed>> = Mutex::new(None);

/// Pause the next sign-in to `user_id` just before its link is stored. The
/// receiver completes once it has paused; it resumes when the sender is
/// dropped or sent to.
#[must_use]
pub fn arm(user_id: &UserId) -> (Receiver<()>, Sender<()>) {
	let (paused, on_pause) = channel();
	let (resume, on_resume) = channel();

	*ARMED.lock().expect("locked") = Some((user_id.to_owned(), paused, on_resume));

	(on_pause, resume)
}

/// Where a sign-in to the armed account pauses.
pub(super) async fn point(user_id: &UserId) {
	let armed = ARMED
		.lock()
		.expect("locked")
		.take_if(|(armed, ..)| &**armed == user_id);

	if let Some((_, paused, resume)) = armed {
		paused.send(()).ok();
		resume.await.ok();
	}
}
