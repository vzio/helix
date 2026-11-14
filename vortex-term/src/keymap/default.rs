use std::collections::HashMap;

use super::macros::keymap;
use super::{KeyTrie, Layer, Mode};
use vortex_core::hashmap;

pub fn default() -> HashMap<Mode, KeyTrie> {
    let normal = keymap!({ "Normal mode"
        "h" | "left" => move_char_left,
        "j" | "down" => move_visual_line_down,
        "k" | "up" => move_visual_line_up,
        "l" | "right" => move_char_right,

        "t" => find_till_char,
        "f" => find_next_char,
        "T" => till_prev_char,
        "F" => find_prev_char,
        "r" => replace,
        "R" => replace_with_yanked,
        "A-." =>  repeat_last_motion,

        "~" => switch_case,
        "`" => switch_to_lowercase,
        "A-`" => switch_to_uppercase,

        "home" => goto_line_start,
        "end" => goto_line_end,

        "w" => move_next_word_start,
        "b" => move_prev_word_start,
        "e" => move_next_word_end,

        "W" => move_next_long_word_start,
        "B" => move_prev_long_word_start,
        "E" => move_next_long_word_end,

        "v" => select_mode,
        "G" => goto_line,
        "g" => { "Goto"
            ["in the file"]
            "g" => goto_file_start,
            "e" => goto_last_line,
            "|" => goto_column,
            "h" => goto_line_start,
            "l" => goto_line_end,
            "s" => goto_first_nonwhitespace,
            "k" => move_line_up,
            "j" => move_line_down,
            "w" => goto_word,
            ["on the screen"]
            "t" => goto_window_top,
            "c" => goto_window_center,
            "b" => goto_window_bottom,
            ["LSP"]
            "d" => goto_definition,
            "D" => goto_declaration,
            "y" => goto_type_definition,
            "r" => goto_reference,
            "i" => goto_implementation,
            ["files"]
            "f" => goto_file,
            "m" => goto_last_modified_file,
            "n" => goto_next_buffer,
            "p" => goto_previous_buffer,
            ["history"]
            "a" => history_toggle,
            "A" => history_back,
            "." => goto_last_modification,
        },
        ":" => command_mode,

        "i" => insert_mode,
        "I" => insert_at_line_start,
        "a" => append_mode,
        "A" => insert_at_line_end,
        "o" => open_below,
        "O" => open_above,

        "d" => delete_selection,
        "A-d" => delete_selection_noyank,
        "c" => change_selection,
        "A-c" => change_selection_noyank,

        "C" => copy_selection_on_next_line,
        "A-C" => copy_selection_on_prev_line,


        "s" => select_regex,
        "A-s" => split_selection_on_newline,
        "A-minus" => merge_selections,
        "A-_" => merge_consecutive_selections,
        "S" => split_selection,
        ";" => collapse_selection,
        "A-;" => flip_selections,
        "A-o" | "A-up" => expand_selection,
        "A-i" | "A-down" => shrink_selection,
        "A-I" | "A-S-down" => select_all_children,
        "A-p" | "A-left" => select_prev_sibling,
        "A-n" | "A-right" => select_next_sibling,
        "A-e" => move_parent_node_end,
        "A-b" => move_parent_node_start,
        "A-a" => select_all_siblings,

        "%" => select_all,
        "x" => extend_line_below,
        "X" => extend_to_line_bounds,
        "A-x" => shrink_to_line_bounds,

        "m" => { "Match"
            "m" => match_brackets,
            "s" => surround_add,
            "r" => surround_replace,
            "d" => surround_delete,
            "a" => select_textobject_around,
            "i" => select_textobject_inner,
        },
        "[" => { "Left bracket"
            ["diagnostics and changes"]
            "d" => goto_prev_diag,
            "D" => goto_first_diag,
            "g" => goto_prev_change,
            "G" => goto_first_change,
            ["syntax"]
            "f" => goto_prev_function,
            "t" => goto_prev_class,
            "a" => goto_prev_parameter,
            "c" => goto_prev_comment,
            "e" => goto_prev_entry,
            "T" => goto_prev_test,
            "p" => goto_prev_paragraph,
            "x" => goto_prev_xml_element,
            ["lines"]
            "space" => add_newline_above,
        },
        "]" => { "Right bracket"
            ["diagnostics and changes"]
            "d" => goto_next_diag,
            "D" => goto_last_diag,
            "g" => goto_next_change,
            "G" => goto_last_change,
            ["syntax"]
            "f" => goto_next_function,
            "t" => goto_next_class,
            "a" => goto_next_parameter,
            "c" => goto_next_comment,
            "e" => goto_next_entry,
            "T" => goto_next_test,
            "p" => goto_next_paragraph,
            "x" => goto_next_xml_element,
            ["lines"]
            "space" => add_newline_below,
        },

        "/" => search,
        "?" => rsearch,
        "n" => search_next,
        "N" => search_prev,
        "*" => search_selection_detect_word_boundaries,
        "A-*" => search_selection,

        "u" => undo,
        "U" => redo,
        "A-u" => earlier,
        "A-U" => later,

        "y" => yank,
        // yank_all
        "p" => paste_after,
        // paste_all
        "P" => paste_before,

        "Q" => record_macro,
        "q" => replay_macro,

        ">" => indent,
        "<" => unindent,
        "=" => format_selections,
        "J" => join_selections,
        "A-J" => join_selections_space,
        "K" => keep_selections,
        "A-K" => remove_selections,

        "," => keep_primary_selection,
        "A-," => remove_primary_selection,

        // "q" => record_macro,
        // "Q" => replay_macro,

        "&" => align_selections,
        "_" => trim_selections,

        "(" => rotate_selections_backward,
        ")" => rotate_selections_forward,
        "A-(" => rotate_selection_contents_backward,
        "A-)" => rotate_selection_contents_forward,

        "A-:" => ensure_selections_forward,

        "esc" => normal_mode,
        "C-b" | "pageup" => page_up,
        "C-f" | "pagedown" => page_down,
        "C-u" => page_cursor_half_up,
        "C-d" => page_cursor_half_down,

        "C-w" => { "Window"
            ["splits"]
            "C-s" | "s" => hsplit,
            "C-v" | "v" => vsplit,
            "f" => goto_file_hsplit,
            "F" => goto_file_vsplit,
            "C-q" | "q" => wclose,
            "C-o" | "o" => wonly,
            "n" => { "New split scratch buffer"
                "C-s" | "s" => hsplit_new,
                "C-v" | "v" => vsplit_new,
            },
            ["focus"]
            "C-w" | "w" => rotate_view,
            "C-h" | "h" | "left" => jump_view_left,
            "C-j" | "j" | "down" => jump_view_down,
            "C-k" | "k" | "up" => jump_view_up,
            "C-l" | "l" | "right" => jump_view_right,
            ["move"]
            "C-t" | "t" => transpose_view,
            "H" => swap_view_left,
            "J" => swap_view_down,
            "K" => swap_view_up,
            "L" => swap_view_right,
        },

        // move under <space>c
        "C-c" => toggle_comments,

        // z family for save/restore/combine from/to sels from register

        // bookmarks (this fork); tab is C-i without the kitty keyboard protocol
        "C-i" | "tab" => next_bookmark,
        "C-o" => prev_bookmark,
        "C-s" => toggle_bookmark,

        "space" => { "Space"
            ["files"]
            "f" => file_picker,
            "F" => file_picker_in_current_directory,
            "e" => toggle_explorer_root,
            "E" => toggle_explorer,
            "." => file_explorer_in_current_buffer_directory,
            "b" => buffer_picker,
            "j" => bookmark_picker,
            "'" => last_picker,
            ["search"]
            "/" => global_search,
            "s" => lsp_or_syntax_symbol_picker,
            "S" => lsp_or_syntax_workspace_symbol_picker,
            "d" => diagnostics_picker,
            "D" => workspace_diagnostics_picker,
            ["git"]
            "g" => changed_file_picker,
            "l" => commit_picker,
            "L" => line_history,
            "=" => git_diff_view,
            "B" => toggle_inline_blame,
            ["LSP"]
            "a" => code_action,
            "k" => hover,
            "r" => rename_symbol,
            "h" => select_references_to_symbol_under_cursor,
            "i" => yank_diagnostic,
            ["clipboard"]
            "y" => yank_to_clipboard,
            "Y" => yank_main_selection_to_clipboard,
            "p" => paste_clipboard_after,
            "P" => paste_clipboard_before,
            "R" => replace_selections_with_clipboard,
            ["comments"]
            "c" => toggle_comments,
            "C" => toggle_block_comments,
            "A-c" => toggle_line_comments,
            ["windows and debug"]
            "w" => { "Window"
                ["splits"]
                "C-s" | "s" => hsplit,
                "C-v" | "v" => vsplit,
                "f" => goto_file_hsplit,
                "F" => goto_file_vsplit,
                "C-q" | "q" => wclose,
                "C-o" | "o" => wonly,
                "n" => { "New split scratch buffer"
                    "C-s" | "s" => hsplit_new,
                    "C-v" | "v" => vsplit_new,
                },
                ["focus"]
                "C-w" | "w" => rotate_view,
                "C-h" | "h" | "left" => jump_view_left,
                "C-j" | "j" | "down" => jump_view_down,
                "C-k" | "k" | "up" => jump_view_up,
                "C-l" | "l" | "right" => jump_view_right,
                ["move"]
                "C-t" | "t" => transpose_view,
                "H" => swap_view_left,
                "J" => swap_view_down,
                "K" => swap_view_up,
                "L" => swap_view_right,
            },
            "G" => debug_mode,
            ["commands"]
            "?" => command_palette,
        },
        "z" => { "View"
            ["align"]
            "z" | "c" => align_view_center,
            "t" => align_view_top,
            "b" => align_view_bottom,
            "m" => align_view_middle,
            ["scroll"]
            "k" | "up" => scroll_up,
            "j" | "down" => scroll_down,
            "C-b" | "pageup" => page_up,
            "C-f" | "pagedown" => page_down,
            "C-u" | "backspace" => page_cursor_half_up,
            "C-d" | "space" => page_cursor_half_down,
            ["search"]
            "/" => search,
            "?" => rsearch,
            "n" => search_next,
            "N" => search_prev,
        },
        "Z" => { "View" sticky=true
            ["align"]
            "z" | "c" => align_view_center,
            "t" => align_view_top,
            "b" => align_view_bottom,
            "m" => align_view_middle,
            ["scroll"]
            "k" | "up" => scroll_up,
            "j" | "down" => scroll_down,
            "C-b" | "pageup" => page_up,
            "C-f" | "pagedown" => page_down,
            "C-u" | "backspace" => page_cursor_half_up,
            "C-d" | "space" => page_cursor_half_down,
            ["search"]
            "/" => search,
            "?" => rsearch,
            "n" => search_next,
            "N" => search_prev,
        },
        "M" => { "Move" sticky=true mode="MOV"
            "j" | "down" => move_lines_down,
            "k" | "up" => move_lines_up,
        },

        "\"" => select_register,
        "|" => shell_pipe,
        "A-|" => shell_pipe_to,
        "!" => shell_insert_output,
        "A-!" => shell_append_output,
        "$" => shell_keep_pipe,
        "C-z" => suspend,
        "C-t" => toggle_terminal,
        "C-g" => debug_panel_focus,

        "C-a" => increment,
        "C-x" => decrement,
    });
    let mut select = normal.clone();
    select.merge_nodes(keymap!({ "Select mode"
        "h" | "left" => extend_char_left,
        "j" | "down" => extend_visual_line_down,
        "k" | "up" => extend_visual_line_up,
        "l" | "right" => extend_char_right,

        "w" => extend_next_word_start,
        "b" => extend_prev_word_start,
        "e" => extend_next_word_end,
        "W" => extend_next_long_word_start,
        "B" => extend_prev_long_word_start,
        "E" => extend_next_long_word_end,

        "A-e" => extend_parent_node_end,
        "A-b" => extend_parent_node_start,

        "n" => extend_search_next,
        "N" => extend_search_prev,

        "t" => extend_till_char,
        "f" => extend_next_char,
        "T" => extend_till_prev_char,
        "F" => extend_prev_char,

        "home" => extend_to_line_start,
        "end" => extend_to_line_end,
        "esc" => exit_select_mode,

        "v" => normal_mode,
        "g" => { "Goto"
            "g" => extend_to_file_start,
            "|" => extend_to_column,
            "e" => extend_to_last_line,
            "k" => extend_line_up,
            "j" => extend_line_down,
            "w" => extend_to_word,
        },
    }));
    let insert = keymap!({ "Insert mode"
        "esc" => normal_mode,

        "C-s" => commit_undo_checkpoint,
        "C-t" => toggle_terminal,
        "C-x" => completion,
        "C-r" => insert_register,

        "C-w" | "A-backspace" => delete_word_backward,
        "A-d" | "A-del" => delete_word_forward,
        "C-u" => kill_to_line_start,
        "C-k" => kill_to_line_end,
        "C-h" | "backspace" | "S-backspace" => delete_char_backward,
        "C-d" | "del" => delete_char_forward,
        "C-j" | "ret" => insert_newline,
        "tab" => smart_tab,
        "S-tab" => insert_tab,

        "up" => move_visual_line_up,
        "down" => move_visual_line_down,
        "left" => move_char_left,
        "right" => move_char_right,
        "pageup" => page_up,
        "pagedown" => page_down,
        "home" => goto_line_start,
        "end" => goto_line_end_newline,
    });
    hashmap!(
        Mode::Normal => normal,
        Mode::Select => select,
        Mode::Insert => insert,
    )
}

/// The keys of the fork's modes (see [`Layer`]): one key per command, shown on the mode bar.
pub fn layers() -> HashMap<Layer, KeyTrie> {
    let watch = keymap!({ "Watch mode"
        "v" => watch_look,
        "n" => watch_next_change,
        "N" => watch_prev_change,
        "c" => watch_changes,
        "esc" => watch_leave,
    });
    // the groups are the `?` box's
    let debug = keymap!({ "Debug mode"
        ["playback"]
        "space" => debug_play_pause,
        "l" => debug_line_forward,
        "h" => debug_line_back,
        "L" => debug_breakpoint_forward,
        "H" => debug_breakpoint_back,
        "k" => dap_faster,
        "j" => dap_slower,
        "p" => dap_live,
        "f" => dap_fast_forward,
        "r" => dap_run_to_cursor,
        "e" => dap_follow_mode,
        ["stepping"]
        "." => dap_next,
        "i" => dap_step_in,
        "o" => dap_step_out,
        ["threads"]
        ">" => dap_next_thread,
        "<" => dap_previous_thread,
        "t" => dap_switch_thread,
        "F" => dap_switch_stack_frame,
        "z" => dap_freeze_thread,
        "a" => dap_solo_thread,
        ["view and breakpoints"]
        "v" => debug_view,
        "b" => dap_toggle_breakpoint,
        "B" => debug_breakpoint_kind,
        "C" => dap_edit_condition,
        "M" => dap_edit_log,
        "E" => dap_toggle_exceptions,
        ["session"]
        "s" => dap_launch,
        "S" => dap_terminate,
        "R" => dap_restart,
        ["inspect"]
        "x" => dap_evaluate,
        "w" => dap_watch,
        "V" => dap_variables,
        "m" => dap_memory,
        "c" => dap_capture_settings,
        "X" => dap_clear_time_labels,
        ["panel"]
        "tab" | "C-g" => debug_panel_focus,
        "P" => debug_panel,
        ["mode"]
        "?" => debug_help,
        "esc" => debug_leave,
    });
    hashmap!(
        Layer::Watch => watch,
        Layer::Debug => debug,
    )
}
