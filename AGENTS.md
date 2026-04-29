# Agent Guidelines

    ## Merging Pull Requests

    **Always use `gh pr merge` — never `git merge` + `git push` directly.**

    ```bash
    gh pr merge <number> --merge   # or --squash / --rebase
    ```

    Using raw git operations puts the code on main correctly but GitHub never learns the PR was merged — it stays CLOSED instead of MERGED. GitHub's MERGED status is only set when the merge goes through its PR mechanism.

    If you accidentally push a branch directly to main, there is no way to retroactively mark the PR as MERGED. GitHub will reject any subsequent merge attempt because the commits are already present in the target branch.
    