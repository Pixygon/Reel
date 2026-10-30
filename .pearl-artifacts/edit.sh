set -e
# 1. doc comment back on poll_autosave
sed -i '/^    \/\/\/ Save the project automatically, shortly after edits stop. Serialising$/,/^    \/\/\/ worker thread, so a slow disk can never stall a frame.$/d' src/app.rs
sed -i 's|^    pub fn poll_autosave(&mut self) {$|    /// Save the project automatically, shortly after edits stop. Serialising\n    /// happens here (it'"'"'s a small JSON document) but the write goes to a\n    /// worker thread, so a slow disk can never stall a frame.\n&|' src/app.rs
