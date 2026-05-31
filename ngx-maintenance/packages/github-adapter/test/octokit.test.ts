import { describe, expect, it } from "vitest";
import {
  parseRepoRef,
  repoUrlMatchesFullName,
  sameRepo,
} from "../src/index.js";

describe("parseRepoRef", () => {
  it("parses an https github URL", () => {
    expect(parseRepoRef("https://github.com/acme/widget")).toEqual({
      owner: "acme",
      repo: "widget",
    });
  });

  it("strips a trailing .git", () => {
    expect(parseRepoRef("https://github.com/acme/widget.git")).toEqual({
      owner: "acme",
      repo: "widget",
    });
  });

  it("parses an ssh remote", () => {
    expect(parseRepoRef("git@github.com:acme/widget.git")).toEqual({
      owner: "acme",
      repo: "widget",
    });
  });

  it("parses a bare owner/repo slug", () => {
    expect(parseRepoRef("acme/widget")).toEqual({
      owner: "acme",
      repo: "widget",
    });
  });

  it("returns undefined when no slug can be recovered", () => {
    expect(parseRepoRef("not-a-repo")).toBeUndefined();
    expect(parseRepoRef("")).toBeUndefined();
  });
});

describe("repoUrlMatchesFullName / sameRepo", () => {
  it("matches across URL / slug / scheme / case differences", () => {
    expect(
      repoUrlMatchesFullName("https://github.com/Acme/Widget.git", "acme/widget"),
    ).toBe(true);
    expect(sameRepo("git@github.com:acme/widget.git", "acme/widget")).toBe(true);
  });

  it("does not match different repos", () => {
    expect(repoUrlMatchesFullName("acme/widget", "acme/other")).toBe(false);
    expect(sameRepo("acme/widget", "garbage")).toBe(false);
  });
});
