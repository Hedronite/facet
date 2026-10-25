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

# Empty or unset FACET_ACTOR uses the CLI default, human (actor_from_env).
# The skill does not invent an actor name.
def facet-actor [] {
  let set = (try { $env.FACET_ACTOR } catch { "" })
  if ($set | is-empty) { "human" } else { $set }
}

def has-collection [dir: string] {
  let yml = (
    ["opencollection.yml" "opencollection.yaml"]
    | any {|name| ($dir | path join $name) | path exists }
  )
  let db = (($dir | path join ".facet" "lattice.db") | path exists)
  $yml or $db
}

def walk-up [dir: string, start: string] {
  if (has-collection $dir) {
    { ok: true, dir: $dir, error: "" }
  } else {
    let parent = ($dir | path dirname)
    if $parent == $dir {
      { ok: false, dir: "", error: $"no collection at ($start)" }
    } else {
      walk-up $parent $start
    }
  }
}

# A path may be the collection directory, its OpenCollection file, or anything
# inside it. With no path, walk up from the working directory. Failure does
# not call facet.
def resolve-collection [path: string] {
  let given = ($path | str trim)
  let start = if ($given | is-empty) { $env.PWD } else { $given }
  if not ($start | path exists) {
    { ok: false, dir: "", error: $"collection not found: ($start)" }
  } else {
    let kind = ($start | path type)
    let anchor = if $kind == "file" { $start | path dirname } else { $start }
    walk-up ($anchor | path expand) $start
  }
}

def run-facet [args: list<string>, collection: string = ""] {
  let blocked = (guard ($args | append $collection))
  if not ($blocked | is-empty) {
    {
      stdout: ""
      stderr: $"refusing flag ($blocked | first)"
      exit_code: 2
      refused: true
    }
  } else {
    let found = (resolve-collection $collection)
    if not $found.ok {
      {
        stdout: ""
        stderr: $found.error
        exit_code: 2
        refused: true
      }
    } else {
      let actor = (facet-actor)
      let done = (with-env { FACET_ACTOR: $actor } {
        cd $found.dir
        ^facet ...$args --json | complete
      })
      $done | insert refused false
    }
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

def envelope [command: string, args: list<string>, collection: string = ""] {
  let r = (run-facet $args $collection)
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
    ++ ["--limit" ($limit | into string)]
    ++ (opt "--request" $request)
    ++ (opt "--status" $status)
    ++ (opt "--session" $session)
  ) $path | emit
}

def "main last" [path: string = "", --request: string = ""] {
  envelope "last" (["last"] ++ (opt "--request" $request)) $path | emit
}

def "main sessions" [path: string = "", --open] {
  let args = if $open { ["session" "list" "--open"] } else { ["session" "list"] }
  envelope "sessions" $args $path | emit
}

def "main session" [id: string, path: string = ""] {
  if ($id | is-empty) {
    fail "session" { stdout: "", stderr: "session requires <id> or current", exit_code: 2, refused: true }
  } else {
    envelope "session" ["session" "show" $id] $path
  } | emit
}

def "main pins" [path: string = ""] {
  envelope "pins" ["pin" "list"] $path | emit
}

def "main env" [path: string = "", --environment: string = ""] {
  let result = (envelope "env" (["env" "list"] ++ (opt "--environment" $environment)) $path)
  if $result.ok {
    $result | upsert data (project-names $result.data)
  } else {
    $result
  } | emit
}

def "main diff" [a: string, b: string, path: string = ""] {
  let r = (run-facet ["diff" $a $b] $path)
  let data = (parsed $r.stdout)
  if $r.refused or $data == null or not ($r.exit_code in [0 1]) {
    fail "diff" $r
  } else {
    { ok: true, command: "diff", same: ($r.exit_code == 0), data: $data }
  } | emit
}

def "main doctor" [path: string = ""] {
  let r = (run-facet ["doctor"] $path)
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
