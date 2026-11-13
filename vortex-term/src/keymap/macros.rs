#[macro_export]
macro_rules! key {
    ($key:ident) => {
        ::vortex_view::input::KeyEvent {
            code: ::vortex_view::keyboard::KeyCode::$key,
            modifiers: ::vortex_view::keyboard::KeyModifiers::NONE,
        }
    };
    ($($ch:tt)*) => {
        ::vortex_view::input::KeyEvent {
            code: ::vortex_view::keyboard::KeyCode::Char($($ch)*),
            modifiers: ::vortex_view::keyboard::KeyModifiers::NONE,
        }
    };
}

#[macro_export]
macro_rules! shift {
    ($key:ident) => {
        ::vortex_view::input::KeyEvent {
            code: ::vortex_view::keyboard::KeyCode::$key,
            modifiers: ::vortex_view::keyboard::KeyModifiers::SHIFT,
        }
    };
    ($($ch:tt)*) => {
        ::vortex_view::input::KeyEvent {
            code: ::vortex_view::keyboard::KeyCode::Char($($ch)*),
            modifiers: ::vortex_view::keyboard::KeyModifiers::SHIFT,
        }
    };
}

#[macro_export]
macro_rules! ctrl {
    ($key:ident) => {
        ::vortex_view::input::KeyEvent {
            code: ::vortex_view::keyboard::KeyCode::$key,
            modifiers: ::vortex_view::keyboard::KeyModifiers::CONTROL,
        }
    };
    ($($ch:tt)*) => {
        ::vortex_view::input::KeyEvent {
            code: ::vortex_view::keyboard::KeyCode::Char($($ch)*),
            modifiers: ::vortex_view::keyboard::KeyModifiers::CONTROL,
        }
    };
}

#[macro_export]
macro_rules! alt {
    ($key:ident) => {
        ::vortex_view::input::KeyEvent {
            code: ::vortex_view::keyboard::KeyCode::$key,
            modifiers: ::vortex_view::keyboard::KeyModifiers::ALT,
        }
    };
    ($($ch:tt)*) => {
        ::vortex_view::input::KeyEvent {
            code: ::vortex_view::keyboard::KeyCode::Char($($ch)*),
            modifiers: ::vortex_view::keyboard::KeyModifiers::ALT,
        }
    };
}

/// Macro for defining a `KeyTrie`. Example:
///
/// ```
/// # use vortex_core::hashmap;
/// # use vortex_term::keymap;
/// let normal_mode = keymap!({ "Normal mode"
///     "i" => insert_mode,
///     "g" => { "Goto"
///         "g" => goto_file_start,
///         "e" => goto_file_end,
///     },
///     "j" | "down" => move_line_down,
/// });
/// let keymap = normal_mode;
/// ```
#[macro_export]
macro_rules! keymap {
    (@trie $cmd:ident) => {
        $crate::keymap::KeyTrie::MappableCommand($crate::commands::MappableCommand::$cmd)
    };

    (@trie
        { $label:literal $(sticky=$sticky:literal)? $(mode=$mode:literal)? $($($key:literal)|+ => $value:tt,)+ }
    ) => {
        keymap!({ $label $(sticky=$sticky)? $(mode=$mode)? $($($key)|+ => $value,)+ })
    };

    (@trie
        { $label:literal $(sticky=$sticky:literal)? $(mode=$mode:literal)? $([$group:literal] $($($key:literal)|+ => $value:tt,)+)+ }
    ) => {
        keymap!({ $label $(sticky=$sticky)? $(mode=$mode)? $([$group] $($($key)|+ => $value,)+)+ })
    };

    (@trie [$($cmd:ident),* $(,)?]) => {
        $crate::keymap::KeyTrie::Sequence(vec![$($crate::commands::MappableCommand::$cmd),*])
    };

    (
        { $label:literal $(sticky=$sticky:literal)? $(mode=$mode:literal)? $($($key:literal)|+ => $value:tt,)+ }
    ) => {
        // modified from the hashmap! macro
        {
            let _cap = hashmap!(@count $($($key),+),*);
            let mut _map = ::indexmap::IndexMap::with_capacity(_cap);
            $(
                $(
                    let _key = $key.parse::<::vortex_view::input::KeyEvent>().unwrap();
                    let _duplicate = _map.insert(
                        _key,
                        keymap!(@trie $value)
                    );
                    assert!(_duplicate.is_none(), "Duplicate key found: {:?}", _duplicate.unwrap());
                )+
            )*
            let mut _node = $crate::keymap::KeyTrieNode::new($label, _map);
            $( _node.is_sticky = $sticky; )?
            $( _node.mode_label = Some($mode); )?
            $crate::keymap::KeyTrie::Node(_node)
        }
    };

    // a menu in groups (this fork): `["files"]` before the keys of a group labels them in the
    // infobox
    (
        { $label:literal $(sticky=$sticky:literal)? $(mode=$mode:literal)? $([$group:literal] $($($key:literal)|+ => $value:tt,)+)+ }
    ) => {
        {
            let mut _map = ::indexmap::IndexMap::new();
            let mut _groups = ::indexmap::IndexMap::new();
            $(
                let _group: &'static str = $group;
                $(
                    $(
                        let _key = $key.parse::<::vortex_view::input::KeyEvent>().unwrap();
                        let _duplicate = _map.insert(
                            _key,
                            keymap!(@trie $value)
                        );
                        assert!(_duplicate.is_none(), "Duplicate key found: {:?}", _duplicate.unwrap());
                        _groups.insert(_key, _group);
                    )+
                )+
            )+
            let mut _node = $crate::keymap::KeyTrieNode::new($label, _map);
            _node.groups = _groups;
            $( _node.is_sticky = $sticky; )?
            $( _node.mode_label = Some($mode); )?
            $crate::keymap::KeyTrie::Node(_node)
        }
    };
}

pub use alt;
pub use ctrl;
pub use key;
pub use keymap;
pub use shift;
