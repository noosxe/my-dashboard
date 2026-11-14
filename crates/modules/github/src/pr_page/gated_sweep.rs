//! #875: the gated-runs sweep spends REST requests, so it obeys the REST
//! pause, and it is booked under the trigger of the read that armed it.

#[cfg(test)]
mod tests;
