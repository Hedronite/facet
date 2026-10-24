#!/usr/bin/env nu
# Deterministic, read-only body for the facet skill.
# allow-flags: --limit --request --status --session --open --environment --json
# `--json` is appended after the subcommand. The first token selects Facet
# or Probe, so `facet --json <command>` never reaches these commands.
# Flag-like arguments are refused. Commands that are not wrapped are listed
# in SKILL.md.

const allowed_flags = ["--limit" "--request" "--status" "--session" "--open" "--environment"]

def clip [text: string] {
  let flat = ($text | lines | str join " " | str trim)
  if ($flat | str length) <= 400 {
    $flat
  } else {
    $flat | str substring 0..<400
  }
}

def guard [args: list<string>] {
  $args | where {|arg|
    ($arg | str starts-with "-") and not ($arg in $allowed_flags)
  }
}

def parsed [stdout: string] {
  try { $stdout | from json } catch { null }
}

def run-facet [args: list<string>] {
  let blocked = (guard $args)
  if not ($blocked | is-empty) {
    {
      stdout: ""
      stderr: $"refusing flag ($blocked | first)"
      exit_code: 2
      refused: true
    }
  } else {
    let done = (^facet ...$args --json | complete)
    $done | insert refused false
  }
}

def fail [command: string, r: record] {
  let data = (parsed $r.stdout)
  let base = {
    ok: false
    command: $command
    error: (clip ([$r.stderr $r.stdout] | str join " "))
    exit: $r.exit_code
  }
  if $data == null { $base } else { $base | insert data $data }
}

def envelope [command: string, args: list<string>] {
  let r = (run-facet $args)
  let data = (parsed $r.stdout)
  if $r.refused or $r.exit_code != 0 or $data == null {
    fail $command $r
  } else {
    { ok: true, command: $command, data: $data }
  }
}

def emit []: record -> any {
  let doc = $in
  if $doc.ok {
    $doc | to json
  } else {
    print ($doc | to json)
    exit ($doc.exit? | default 1)
  }
}

def opt [flag: string, v: string] { if ($v | is-empty) { [] } else { [$flag $v] } }
def pos [v: string] { if ($v | is-empty) { [] } else { [$v] } }

# `{environment, name}` only. Drops every other entry field.
def project-names [data: record] {
  let entries = (
    $data.entries? | default [] | each {|row|
      {
        environment: ($row.environment? | default null)
        name: ($row.name? | default null)
      }
    }
  )
  {
    schemaVersion: ($data.schemaVersion? | default 1)
    entries: $entries
  }
}

def "main history" [
  path: string = ""
  --limit: int = 20
  --request: string = ""
  --status: string = ""
  --session: string = ""
] {
  envelope "history" (
    ["history"]
    ++ (pos $path)
    ++ ["--limit" ($limit | into string)]
    ++ (opt "--request" $request)
    ++ (opt "--status" $status)
    ++ (opt "--session" $session)
  ) | emit
}

def "main last" [path: string = "", --request: string = ""] {
  envelope "last" (["last"] ++ (pos $path) ++ (opt "--request" $request)) | emit
}

def "main sessions" [--open] {
  let args = if $open { ["session" "list" "--open"] } else { ["session" "list"] }
  envelope "sessions" $args | emit
}

def "main session" [id: string] {
  if ($id | is-empty) {
    fail "session" { stdout: "", stderr: "session requires <id> or current", exit_code: 2, refused: true }
  } else {
    envelope "session" ["session" "show" $id]
  } | emit
}

def "main pins" [path: string = ""] {
  envelope "pins" (["pin" "list"] ++ (pos $path)) | emit
}

def "main env" [path: string = "", --environment: string = ""] {
  let result = (envelope "env" (["env" "list"] ++ (pos $path) ++ (opt "--environment" $environment)))
  if $result.ok {
    $result | upsert data (project-names $result.data)
  } else {
    $result
  } | emit
}

def "main diff" [a: string, b: string, path: string = ""] {
  let r = (run-facet (["diff" $a $b] ++ (pos $path)))
  let data = (parsed $r.stdout)
  if $r.refused or $data == null or not ($r.exit_code in [0 1]) {
    fail "diff" $r
  } else {
    { ok: true, command: "diff", same: ($r.exit_code == 0), data: $data }
  } | emit
}

def "main doctor" [] {
  let r = (run-facet ["doctor"])
  let data = (parsed $r.stdout)
  let warnings = (
    $r.stderr
    | lines
    | each {|line| $line | str trim}
    | where {|line| not ($line | is-empty)}
  )
  if $r.refused or $data == null or not ($r.exit_code in [0 1]) {
    fail "doctor" $r
  } else {
    {
      ok: true
      command: "doctor"
      healthy: ($r.exit_code == 0)
      warnings: $warnings
      data: $data
    }
  } | emit
}

def main [] { help main }
