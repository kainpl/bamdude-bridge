// A GUI app that also runs headless. `windows_subsystem = "windows"` keeps a
// console window from flashing up every time the slicer hands us a file —
// which, in normal use, is the only way this binary is ever started.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    bamdude_bridge_lib::run()
}
