//! #872: a confirmed review batch refetches the detail as well as the
//! threads. The submitted review changes the detail's `reviews`, the
//! review cards on Conversation, `reviewDecision` and the review count;
//! the Files tab's threads are only one of the surfaces that read it.

#[cfg(test)]
mod tests;
