//! A hole can be a teardrop (`"hole": {"type": "teardrop", "apex", "angle", "cap"}`): the
//! cross-section for a hole printed across the build direction, peaked towards the printer's up.
//! Older designs need no transformation: a hole without it is round as before. The last release
//! refuses a design saved with it (the newer format number).
