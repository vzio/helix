#!/usr/bin/env fish
# Fish completion script for Vortex editor

complete -c vx -s h -l help -d "Prints help information"
complete -c vx -l strict -d "Bail on error for commands that can fail"
complete -c vx -l tutor -d "Loads the tutorial"
complete -c vx -l health -xa "(__vx_langs_ops)" -d "Checks for errors"
complete -c vx -l health -xka all -d "Prints all diagnostic informations"
complete -c vx -l health -xka all-languages -d "Lists all languages"
complete -c vx -l health -xka languages -d "Lists user configured languages"
complete -c vx -l health -xka clipboard -d "Prints system clipboard provider"
complete -c vx -s g -l grammar -x -a "fetch build" -d "Fetch or build tree-sitter grammars"
complete -c vx -s v -o vv -o vvv -d "Increases logging verbosity"
complete -c vx -s V -l version -d "Prints version information"
complete -c vx -l vsplit -d "Splits all given files vertically"
complete -c vx -l hsplit -d "Splits all given files horizontally"
complete -c vx -s c -l config -r -d "Specifies a file to use for config"
complete -c vx -l log -r -d "Specifies a file to use for logging"
complete -c vx -s w -l working-dir -d "Specify initial working directory" -xa "(__fish_complete_directories)"

function __vx_langs_ops
    vx --health all-languages | tail -n '+2' | string replace -fr '^(\S+) .*' '$1'
end
