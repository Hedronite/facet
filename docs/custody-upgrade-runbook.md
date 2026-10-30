# Root-custody upgrade runbook

Use this to upgrade a `facet` binary installed under a root-owned prefix (`<prefix>`), where the live binary is the symlink `<prefix>/bin/facet` → `<prefix>/facet/<version>/facet` and the command allowlist lives at `<prefix>/etc/allow-cmds.json`. Every version stays in its own directory, so rollback is a symlink switch.

## Run order

1. **Verify the release.** `gh attestation verify <archive> --repo Hedronite/facet` must pass. After you extract the archive, record `shasum -a 256 facet` and check it against the attested release digest (`SHA256SUMS`, which is attested too).
2. **Install under the root prefix.** Put the binary at `$D/facet/<version>/facet` (root-owned) and stage the allowlist that matches the version.
3. **Lock and switch** (block below).
4. **Post-checks** (below). Do not go on until every check passes.
5. **Smokes and QA.** Run `facet --version`, `facet doctor --json`, and your usual smoke runs against the live path.
6. **Announce downstream.** Tell dependent hosts or users that `<version>` is live.

## Lock and switch

```bash
D=<prefix>                          # root-owned install prefix
A=<staged allow-cmds.json for <version>>
sudo chflags schg $D/facet/<version>/facet          # make the new binary system-immutable so nothing can modify it in place
sudo ln -sfh $D/facet/<version>/facet $D/bin/facet  # atomic switch of the live symlink; the previous version stays on disk for rollback
sudo install -o root -g wheel -m 0444 $A $D/etc/allow-cmds.json  # version-matched allowlist, root-owned and read-only
sudo -k                                             # drop the cached sudo credential
ls -l $D/bin/facet                                  # quick look at where the link points
```

The symlink and the allowlist must move together. If only the symlink moves, the host is left **half-switched**: the new binary runs against the old version's allowlist.

## Post-checks

```bash
readlink $D/bin/facet                          # must print $D/facet/<version>/facet
ls -lO $D/facet/<version>/facet                # flags column must show schg
shasum -a 256 $D/etc/allow-cmds.json "$A"      # the two digests must match
shasum -a 256 $D/facet/<version>/facet         # must match the attested release digest
```

**If any check fails, stop.** Fix it (most often by rerunning the `install` line) and run every post-check again before you start the smokes.

## Rollback

```bash
sudo ln -sfh $D/facet/<previous-version>/facet $D/bin/facet
sudo install -o root -g wheel -m 0444 <allow-cmds.json for <previous-version>> $D/etc/allow-cmds.json
sudo -k
```

Then run the post-checks again, using `<previous-version>` and its allowlist.

Rollback does not need `sudo chflags noschg`. You only need that to remove or delete a version directory.

## Platform note

`chflags`, `ls -lO`, and `ln -h` are BSD/macOS forms, and `wheel` is the macOS root group. This runbook covers macOS root custody only.

See also: [Install](install.md).
