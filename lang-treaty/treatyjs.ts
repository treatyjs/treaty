import {LRLanguage, LanguageSupport} from "@codemirror/language"
import {html} from "@codemirror/lang-html"
import {javascriptLanguage} from "@codemirror/lang-javascript"
import {cssLanguage} from "@codemirror/lang-css"
import {styleTags, tags as t} from "@lezer/highlight"
import {parseMixed, SyntaxNodeRef, Input} from "@lezer/common"
import {parser} from "./treatyjs.grammar"

const exprParser = javascriptLanguage.parser.configure({
  top: "SingleExpression"
})

const cssParser = cssLanguage.parser.configure({
  top: "StyleSheet"
})

const baseParser = parser.configure({
  props: [
    styleTags({
      Text: t.content,
      Is: t.definitionOperator,
      AttributeName: t.attributeName,
      "AttributeValue ExpressionAttributeValue StatementAttributeValue": t.attributeValue,
      Entity: t.character,
      InvalidEntity: t.invalid,
      "BoundAttributeName/Identifier": t.attributeName,
      "EventName/Identifier": t.special(t.attributeName),
      "ReferenceName/Identifier": t.variableName,
      "DirectiveName/Identifier": t.keyword,
      "{{ }}": t.brace,
      "( )": t.paren,
      "[ ]": t.bracket,
      "# '*'": t.punctuation,
      StyleSection: t.processingInstruction,
      ScriptSection: t.processingInstruction,
      TemplateSection: t.processingInstruction
    })
  ]
})

const exprMixed = {parser: exprParser}
const statementMixed = {parser: javascriptLanguage.parser}
const cssMixed = {parser: cssParser}

const textParser = baseParser.configure({
  wrap: parseMixed((node, input) => node.name == "InterpolationContent" ? exprMixed : null),
})

const attrParser = baseParser.configure({
  wrap: parseMixed((node, input) => node.name == "InterpolationContent" ? exprMixed
    : node.name != "AttributeInterpolation" ? null
    : node.node.parent?.name == "StatementAttributeValue" ? statementMixed : exprMixed),
  top: "Attribute"
})

const textMixed = {parser: textParser}
const attrMixed = {parser: attrParser}

const baseHTML = html()

function mkTreaty(language: LRLanguage) {
  return language.configure({wrap: parseMixed(mixTreaty)}, "treaty")
}

/// A language provider for Treaty files.
export const treatyLanguage = mkTreaty(baseHTML.language as LRLanguage)

function mixTreaty(node: SyntaxNodeRef, input: Input) {
  switch (node.name) {
    case "StyleSection":
      return cssMixed
    case "ScriptSection":
      return statementMixed
    case "TemplateSection":
      return textMixed
    case "Attribute":
      return /^[*#(\[]|\{\{/.test(input.read(node.from, node.to)) ? attrMixed : null
    case "Text":
      return textMixed
  }
  return null
}

/// Treaty language support.
export function treaty(config: {
  /// Provide an HTML language configuration to use as a base. _Must_
  /// be the result of calling `html()` from `@codemirror/lang-html`,
  /// not just any `LanguageSupport` object.
  base?: LanguageSupport
} = {}) {
  let base = baseHTML
  if (config.base) {
    if (config.base.language.name != "html" || !(config.base.language instanceof LRLanguage))
      throw new RangeError("The base option must be the result of calling html(...)")
    base = config.base
  }
  return new LanguageSupport(
    base.language == baseHTML.language ? treatyLanguage : mkTreaty(base.language as LRLanguage),
    [base.support, base.language.data.of({
      closeBrackets: {brackets: ["[", "{", '"']},
      indentOnInput: /^\s*[\}\]]$/
    })])
}