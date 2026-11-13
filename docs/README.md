# Documentation

| Document | What it covers |
| --- | --- |
| [commands.md](commands.md) | Every command, its flags, its exit codes, its side effects. |
| [client-setup.md](client-setup.md) | OpenAI SDK, OpenCode, Cline, Continue, curl. |
| [configuration.md](configuration.md) | The config file, the files on disk, the rename migration. |
| [how-it-works.md](how-it-works.md) | Routing, translation, token refresh, key handling, the auth rules. |
| [updating.md](updating.md) | How `eggpass update` verifies and replaces itself. |
| [troubleshooting.md](troubleshooting.md) | Symptoms, causes, fixes. |
| [releasing.md](releasing.md) | How a release is cut and signed off. Maintainers. |
| [plans/](plans/) | The research, the plans and what actually shipped. |

## Where to start

| You want to… | Read |
| --- | --- |
| Run it | The [README](../README.md) — it has a quick start. |
| Use a specific client | [client-setup.md](client-setup.md) |
| Change what it serves | [configuration.md](configuration.md) |
| Understand why it behaves like this | [how-it-works.md](how-it-works.md) |
| Fix something that is broken | [troubleshooting.md](troubleshooting.md) |
| Ship a new version | [releasing.md](releasing.md) |

## The plans

`docs/plans/` is the reasoning behind the implementation: the research that
was done, the decisions that were locked, the plan that was followed, and the
record of what actually shipped — including where the build departed from the
plan and why.

It is kept in the repository because a proxy that quietly translates between
two providers' APIs is not a thing anyone should have to take on trust.
