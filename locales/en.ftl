core-ready = Core is ready (version { $version }).
test-english-only = English only
cli-usage = Usage: sayraap [--lang ar|en] [--url URL] [--model NAME] [--timeout SECONDS] ask "your question"
llm-unreachable = Could not reach the model at { $url }. Check that the server is running.
llm-http-error = The model server answered with error { $status }.
llm-bad-response = The model server sent a reply that could not be read.
llm-tokens = [{ $model }] tokens: { $prompt } in, { $completion } out.
