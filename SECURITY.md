# Security Policy

## Supported versions

ParadoxCode has not published a stable release. Security fixes currently target the latest commit on
the default branch. A supported-version table will be added with the first public release.

## Reporting a vulnerability

Please do not open a public issue for a suspected vulnerability. Use
[GitHub private vulnerability reporting](https://github.com/danxiaogu520/ParadoxCode/security/advisories/new)
when it is available for the repository. If private reporting is unavailable, open a minimal public
issue asking the maintainers for a private contact channel without including exploit details.

Include, when possible:

- the affected commit or version;
- the platform and editor involved;
- clear reproduction steps or a proof of concept;
- the expected impact;
- whether the issue is already public.

The maintainers will acknowledge a complete report as soon as practical, keep the reporter informed
of material progress, and coordinate disclosure after a fix is available. Please allow reasonable
time for investigation before publishing details.

## Security boundaries

ParadoxCode treats Mod files, workspace configuration, rules artifacts, downloaded binaries, and
filesystem events as untrusted inputs. The project does not execute Mod or CWT content as code.
Release downloads must be versioned and checksum-verified before automatic installation is enabled.
