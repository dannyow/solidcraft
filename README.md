# dannyow/solidcraft

This is my fork of [tangency-labs/solidcraft](https://github.com/tangency-labs/solidcraft). If you are looking for the SolidCraft project, go there.

The SolidCraft code in this fork is on `main`, which mirrors upstream, and on the feature branches. This branch, `gate`, holds only a workflow file. It is the default branch because GitHub starts a manually dispatched workflow only from the default branch.

The workflow, Gate, runs upstream's CI on a chosen branch of this fork before a pull request goes upstream. It is generated from upstream's `.github/workflows/ci.yml` and runs the same jobs on GitHub-hosted runners instead of upstream's Blacksmith runners. Timeouts are longer to suit the smaller runners, and the Blacksmith cache disks are left out.

To run it, open the Actions tab, choose Gate, then Run workflow. Set `ref` to the branch or commit to check. The `nightly` option adds the jobs upstream runs only once a day, and `cross_compile` adds the WebAssembly and RISC-V checks.
