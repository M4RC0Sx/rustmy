// semantic-release: the next version, CHANGELOG.md and the GitHub release,
// from the Conventional Commit titles of the squash-merged pull requests that
// reach master (.github/workflows/release.yml runs it).
//
// The version lives in [workspace.package] of Cargo.toml (pyproject.toml
// declares it dynamic; maturin copies it into the wheels), so the release
// commit carries Cargo.toml and Cargo.lock, written by scripts/set_version.py.
//
// With RELEASE_PLAN=1 only the analysis runs and the next version is written
// to .release-version: the workflow's plan step, which builds and tests the
// wheels with that version before anything is tagged or published.

const planOnly = process.env.RELEASE_PLAN === "1";

const commitAnalyzer = [
  "@semantic-release/commit-analyzer",
  {
    preset: "conventionalcommits",
    // Custom rules win over the preset's defaults, and among matching rules
    // the highest release wins: the breaking rule must stay explicit, or a
    // breaking `chore!:` would match only `release: false`.
    releaseRules: [
      { breaking: true, release: "major" },
      { type: "feat", release: "minor" },
      { type: "fix", release: "patch" },
      { type: "perf", release: "patch" },
      { type: "refactor", release: "patch" },
      { type: "build", release: "patch" },
      { type: "revert", release: "patch" },
      // The README is the package's page on PyPI, updated only by a release.
      { type: "docs", release: "patch" },
      // Nothing users get: these wait for the next release (and appear in
      // its changelog).
      { type: "test", release: false },
      { type: "ci", release: false },
      { type: "chore", release: false },
      { type: "style", release: false },
    ],
  },
];

const exec = [
  "@semantic-release/exec",
  {
    verifyReleaseCmd: 'echo "${nextRelease.version}" > .release-version',
    prepareCmd: "uv run --no-project scripts/set_version.py ${nextRelease.version}",
  },
];

const releaseNotes = [
  "@semantic-release/release-notes-generator",
  {
    preset: "conventionalcommits",
    presetConfig: {
      types: [
        { type: "feat", section: "Features" },
        { type: "fix", section: "Bug Fixes" },
        { type: "perf", section: "Performance Improvements" },
        { type: "revert", section: "Reverts" },
        { type: "docs", section: "Documentation" },
        { type: "refactor", section: "Code Refactoring" },
        { type: "build", section: "Build System" },
        { type: "test", section: "Tests" },
        { type: "ci", section: "Continuous Integration" },
        { type: "chore", section: "Miscellaneous Chores" },
        { type: "style", section: "Styles" },
      ],
    },
  },
];

export default {
  branches: ["master"],
  tagFormat: "v${version}",
  plugins: planOnly
    ? [commitAnalyzer, exec]
    : [
        commitAnalyzer,
        releaseNotes,
        [
          "@semantic-release/changelog",
          {
            changelogFile: "CHANGELOG.md",
            changelogTitle:
              "# Changelog\n\nAll notable changes to rustmy are documented in this file. " +
              "Versions follow [Semantic Versioning](https://semver.org) and entries come " +
              "from [Conventional Commits](https://www.conventionalcommits.org).",
          },
        ],
        exec,
        [
          "@semantic-release/git",
          {
            assets: ["CHANGELOG.md", "Cargo.toml", "Cargo.lock"],
            message: "chore(release): ${nextRelease.version} [skip ci]\n\n${nextRelease.notes}",
          },
        ],
        [
          "@semantic-release/github",
          {
            assets: [
              { path: "dist/*.whl", label: "Wheels" },
              { path: "dist/*.tar.gz", label: "Source distribution" },
            ],
          },
        ],
      ],
};
