//! Owns wgpu rendering from atlas data and draw lists. Does not change terminal
//! state, discover fonts, depend on `sumie-font`, allocate GPU objects per frame,
//! or submit GPU commands while holding a terminal lock.
