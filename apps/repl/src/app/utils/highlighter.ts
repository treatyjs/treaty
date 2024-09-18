//credit https://github.com/JeanMeche/angular-compiler-output
import { createHighlighter, HighlighterGeneric } from "shiki";

export let highlighter: HighlighterGeneric<any, any> | null = null;

export async function initHightlighter() {
    highlighter = await createHighlighter({
    themes: ['github-dark'],
    langs: ['javascript'],
  });
}
