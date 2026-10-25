# In-tree World fixture (G5a). Exports cluster + intent + calls for facet ncl export.

let lib = import "lib.ncl" in
let World = import "../contracts/world.ncl" in
{
  cluster = [ lib.supported_pod { name = "supported-pod" } ],
  intent = [
    {
      name = "nickel-g5-docs-eod",
      importance = 0.5,
      spec = {
        kind = "docs_eod",
        date = "2026-09-12",
        required_briefs = [ "docs/CHECKLIST-nickel" ],
      },
    },
  ],
  calls = {
    opencollection = "1.0.0",
    info = {
      name = "hedronetes-world-fixture",
      summary = "G5a in-tree World export (cluster + intent + calls)",
    },
    config = {
      environments = [
        {
          name = "lima",
          variables = [ { name = "FACET_PROBE", secret_ref = "kr:probe" } ],
        },
      ],
    },
  },
} | World
