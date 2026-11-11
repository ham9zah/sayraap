core-ready = Core is ready (version { $version }).
test-english-only = English only
cli-usage =
    Usage: sayraap [options] <command>
    Commands:
      ask "question"      send a question to the model and print the answer
      prompt <card.toml>  print the task card as the model would receive it
      run <card.toml>     run the task card: one saved step, or nothing changed
      tasks               list this project's recorded tasks
      attempts <task>     show every attempt of a task (add --show-reply for raw replies)
    Options: --lang ar|en, --project-lang ar|en, --dir PATH, --max-lines N, --check "COMMAND", --check-timeout SECONDS, --db FILE, --url URL, --model NAME, --timeout SECONDS
llm-unreachable = Could not reach the model at { $url }. Check that the server is running.
llm-http-error = The model server answered with error { $status }.
llm-bad-response = The model server sent a reply that could not be read.
llm-tokens = [{ $model }] tokens: { $prompt } in, { $completion } out.

## Task card. The marker lines (SEARCH, =======, REPLACE, MISSING:) are read by code: never translate them.
card-prompt =
    You are making one small change to one file. Follow this card exactly.

    TASK
    { $goal }

    FILE (the only file you may change): { $file }
    CURRENT CONTENT STARTS
    { $current }
    CURRENT CONTENT ENDS

    FORBIDDEN
    { $forbidden }

    ACCEPTANCE CRITERIA
    { $acceptance }

    OUTPUT FORMAT
    Reply with one or more edit blocks and nothing else. Every block has exactly this shape, with each of the three marker lines used once:
    <<<<<<< SEARCH
    lines copied exactly from the current content
    =======
    the lines that replace them
    >>>>>>> REPLACE

    RULES
    1. The SEARCH part must match the current content character for character and appear in it only once. Keep it short.
    2. To ADD new text to a file that has content: copy one existing line next to where the new text goes into SEARCH, then write that same line together with the new text in the replacement. Adding text is always possible this way.
    3. If the current content above is empty, the file is new: leave the SEARCH part empty and write the whole file in the replacement.
    4. Change nothing that the task does not ask for.

    Only if the task needs a fact that is not written on this card (such as a name or a value you would have to invent), do not guess. Reply with this single line instead:
    MISSING: the fact you need

card-prompt-new =
    You are creating one new file. Follow this card exactly.

    TASK
    { $goal }

    FILE TO CREATE: { $file }
    The file does not exist yet. There is no current content, and you do not need any.

    FORBIDDEN
    { $forbidden }

    ACCEPTANCE CRITERIA
    { $acceptance }

    OUTPUT FORMAT
    Reply with the complete content of the new file inside one code block (a line of three backticks before it and a line of three backticks after it) and nothing else.

    Only if the task needs a fact that is not written on this card (such as a name or a value you would have to invent), do not guess. Reply with this single line instead:
    MISSING: the fact you need

card-unreadable = The task card could not be read: { $detail }
card-no-goal = The task card has an empty goal.
card-no-acceptance = The task card has no acceptance criteria.
card-bad-path = The task card names a file outside the project: "{ $file }".
file-unreadable = Could not read { $path }.
file-unwritable = Could not write { $path }.
edit-unreadable = The model's reply did not follow the output format.
edit-not-found = Edit { $index }: the text to find is not in the file.
edit-ambiguous = Edit { $index }: the text to find appears more than once.
edit-no-change = The model's edits leave the file unchanged.
task-missing-info = The model stopped and asked for: { $detail }
task-accepted = Accepted: { $file } changed by { $lines ->
        [one] one line
       *[other] { $lines } lines
    }, saved as step { $commit }.
task-rejected = Rejected. The project is exactly as it was.
task-too-large = The change touches { $changed } lines; the limit is { $limit }.
task-out-of-scope = Files outside the task changed: { $files }
repo-not-found = { $path } is not a project with saved steps (no Git repository at its root).
repo-no-commit = The project has no saved step yet, so there is nothing to return to.
repo-dirty = The project has unsaved changes ({ $files }). Save or discard them first; the task was not started.
repo-failed = The step history reported an error: { $detail }
task-check-failed =
    The check command found a problem with the change:
    { $detail }
task-check-timeout = The check command did not finish within { $seconds } seconds.
check-unavailable = The check command could not be started: { $command }. The task was not judged and nothing was changed.

task-recorded = Recorded as task { $task }, attempt { $attempt }.
task-line = { $id }  [{ $status }]  { $file }  attempts: { $attempts }  step: { $commit }  { $goal }
tasks-empty = No tasks recorded for this project yet.
task-not-found = This project has no task "{ $id }".
attempt-line = Attempt { $number } [{ $model }] { $outcome } — tokens { $prompt } in, { $completion } out, { $seconds } s
status-pending = waiting
status-running = running
status-checking = checking
status-accepted = accepted
status-rejected = rejected
status-stuck = stuck
status-cancelled = cancelled
outcome-running = unfinished
outcome-accepted = accepted
outcome-rejected = rejected
outcome-error = could not be judged
store-failed = The task database reported an error: { $detail }
store-no-folder = No application-data folder was found for the task database. Pass one with --db FILE.
