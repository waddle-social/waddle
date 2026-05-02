pub const AI_PROMPT_CASES: &[(&str, bool, &str)] = &[
    ("/ai summarize", true, "summarize"),
    ("/AI @WADDLE summarize", true, "summarize"),
    ("@wAdDlE continue", true, "continue"),
    ("@waddle_bot continue", false, "@waddle_bot continue"),
    ("@waddleBot continue", false, "@waddleBot continue"),
    ("prefix /ai", false, "prefix /ai"),
    ("☃ /ai later", false, "☃ /ai later"),
    ("/airship @WADDLE", true, "/airship @WADDLE"),
];
