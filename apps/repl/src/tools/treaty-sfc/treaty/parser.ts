import type { Node, HtmlParser } from '@angular/compiler'
import { loadEsmModule } from '@angular-devkit/build-angular/src/utils/load-esm.js';


class HtmlTokenizer {
  private input: string;
  private pos: number;
  private htmlParser?: HtmlParser

  constructor(input: string) {
    this.input = input;
    this.pos = 0;
  }

  async setup() {
    if (!this.htmlParser) {
      const { HtmlParser} = await loadEsmModule<
            typeof import('@angular/compiler')
          >('@angular/compiler')
      this.htmlParser = new HtmlParser()
    }
  }

  private peek(): string | undefined {
    return this.input[this.pos];
  }

  private lookahead(expected: string): boolean {
    return this.input.slice(this.pos).startsWith(expected);
  }

  private advance(by: number): void {
    this.pos += by;
  }

  private consumeWhitespace(): void {
    while (this.peek() && this.peek()?.match(/\s/)) {
      this.advance(1);
    }
  }

  private captureUntil(until: string): string {
    const start = this.pos;
    while (this.peek() && !this.lookahead(until)) {
      this.advance(1);
    }
    return this.input.slice(start, this.pos);
  }

  private parseStyle(): string {
    const start = this.pos;
    while (this.peek() && !this.lookahead('</style>')) {
      this.advance(1);
    }
    const styleContent = this.input.slice(start, this.pos);
    this.advance(8);
    return styleContent;
  }

  private parseJavascript(): string {
    const start = this.pos;
    while (this.peek() && !this.lookahead('---')) {
      this.advance(1);
    }
    const javascriptContent = this.input.slice(start, this.pos);
    this.advance(3);
    return javascriptContent.trim();
  }

  private parseText(): string {
    const start = this.pos;
    while (
      this.peek() &&
      !this.lookahead('<') &&
      !(this.peek() === '{' && this.lookahead('{{'))
    ) {
      this.advance(1);
    }
    const text = this.input.slice(start, this.pos);
    return text.trim();
  }

  // private parseControlFlow() {
  //   const keyword = this.peekKeyword();
  //   if (keyword === 'if') {
  //     return this.parseIf();
  //   } else if (keyword === 'for') {
  //     return this.parseFor();
  //   } else if (keyword === 'switch') {
  //     return this.parseSwitch();
  //   }
  //   return undefined;
  // }

  // private parseIf() {
  //   this.consumeKeyword('if');
  //   const condition = this.captureUntil('{');
  //   this.advance(1);
  //   const content = this.captureBlock();
  //   this.advance(1);

  //   let elseContent;
  //   if (this.lookahead('@else')) {
  //     this.advance(5);
  //     if (this.lookahead(' if')) {
  //       this.advance(3);
  //       elseContent = this.parseIf();
  //     } else {
  //       this.advance(1);
  //       const elseBlock = this.captureBlock();
  //       this.advance(1);
  //       elseContent = {
  //         type: 'if_else',
  //         condition: '',
  //         content: elseBlock,
  //         elseContent: undefined,
  //       };
  //     }
  //   }

  //   return {
  //     type: 'if_else',
  //     condition,
  //     content,
  //     elseContent,
  //   };
  // }

  // private parseFor() {
  //   this.consumeKeyword('for');
  //   const iterator = this.captureUntil('{');
  //   this.advance(1);
  //   const content = this.captureBlock();
  //   this.advance(1);

  //   let emptyContent;
  //   if (this.lookahead('@empty')) {
  //     this.consumeKeyword('empty');
  //     emptyContent = this.captureBlock();
  //   }

  //   return {
  //     type: 'for',
  //     iterator,
  //     content,
  //     emptyContent,
  //   } as any;
  // }

  // private parseSwitch(): ControlFlowContent | undefined {
  //   this.consumeKeyword('switch');
  //   const expression = this.captureUntil('{');
  //   this.advance(1);

  //   const cases: [string, HtmlContent[]][] = [];
  //   let defaultCase;
  //   while (this.peekKeyword()) {
  //     const keyword = this.peekKeyword();
  //     if (keyword === 'case') {
  //       this.consumeKeyword('case');
  //       const caseExpr = this.captureUntil('{');
  //       this.advance(1);
  //       const caseContent = this.captureBlock();
  //       this.advance(1);
  //       cases.push([caseExpr, caseContent]);
  //     } else if (keyword === 'default') {
  //       this.consumeKeyword('default');
  //       this.advance(1);
  //       defaultCase = this.captureBlock();
  //       this.advance(1);
  //       break;
  //     } else {
  //       break;
  //     }
  //   }

  //   return {
  //     type: 'switch',
  //     expression,
  //     cases,
  //     defaultCase,
  //   } as any;
  // }

  private consumeKeyword(keyword: string): void {
    if (this.input.slice(this.pos).startsWith(keyword)) {
      this.pos += keyword.length;
      this.consumeWhitespace();
    }
  }

  private peekKeyword(): string | undefined {
    const currentPos = this.pos;
    let keyword = '';
    while (
      this.peek() &&
      this.peek()?.match(/\S/) &&
      !this.peek()?.match(/\s/)
    ) {
      keyword += this.peek();
      this.advance(1);
    }
    this.pos = currentPos;
    if (keyword) {
      return keyword;
    }
    return undefined;
  }

  private async acaptureBlock() {
    const blockContents: Token[] = [];
    let blockDepth = 1;
    const blockStart = this.pos;

    while (this.peek()) {
      const ch = this.peek();
      if (ch === '{') {
        blockDepth += 1;
        this.advance(1);
      } else if (ch === '}') {
        blockDepth -= 1;
        if (blockDepth === 0) {
          const content = this.input.slice(blockStart, this.pos).trim();
          if (content) {
            await this.setup();
            const result = this.htmlParser!.parse(content, 'template', {
              // Allows for ICUs to be parsed.
              tokenizeExpansionForms: true,
              // Explicitly disable blocks so that their characters are treated as plain text.
              tokenizeBlocks: false,
            })
        
            blockContents.push({
              type: 'html',
              value: result.rootNodes,
            });
          }
          this.advance(1);
          break;
        }
        this.advance(1);
      } else {
        this.advance(1);
      }
    }

    if (blockDepth !== 0) {
      throw new Error('Unmatched braces in block content');
    }

    return blockContents;
  }


  private advanceWhile(condition: (char: string) => boolean) {
    while (this.peek()) {
      const ch = this.peek()!;
      if (condition(ch)) {
        this.advance(1);
      } else {
        break;
      }
    }
  }

  private async parseHtmlSegment() {
    let tagDepth = 0;
    let start = this.pos;
    let end = this.pos;
    let isInTag = false;

    while (this.peek()) {
      const ch = this.peek();
      if (ch === '<') {
        isInTag = true;
        if (this.lookahead('</')) {
          tagDepth -= 1;
          if (tagDepth === 0) {
            this.advanceWhile((c) => c !== '>');
            this.advance(1);
            end = this.pos;
            break;
          }
        } else if (this.lookahead('/>') || this.lookahead('>')) {
          if (tagDepth === 0) {
            end = this.pos + 1;
            this.advance(1);
            break;
          }
        } else {
          if (tagDepth === 0) {
            start = this.pos;
          }
          tagDepth += 1;
        }
      } else if (ch === '>' && isInTag) {
        isInTag = false;
        if (tagDepth === 0) {
          end = this.pos + 1;
          this.advance(1);
          break;
        }
      }
      this.advance(1);
      if (tagDepth === 0 && !isInTag) {
        end = this.pos;
      }
    }

    const htmlContent = this.input.slice(start, end);
    console.log(`Processing HTML segment: ${htmlContent}`);
    await this.setup()
    const result = this.htmlParser!.parse(htmlContent, 'template', {
      // Allows for ICUs to be parsed.
      tokenizeExpansionForms: true,
      // Explicitly disable blocks so that their characters are treated as plain text.
      tokenizeBlocks: false,
    })

    return {
      type: 'html',
      value: result.rootNodes,
    };
  }


  private parseTemplateExpression(): string | undefined {
    const start = this.pos;
    while (this.peek()) {
      if (this.peek() === '}' && this.lookahead('}}')) {
        const expression = this.input.slice(start, this.pos);
        this.advance(2);
        return expression.trim();
      }
      this.advance(1);
    }
    return undefined;
  }

  private async nextToken() {
    while (this.peek()) {
      const currentChar = this.peek();
      switch (currentChar) {
        case '<':
          if (this.lookahead('<style>')) {
            this.advance(7);
            return {
              type: 'style',
              value: this.parseStyle(),
            };
          } else {
            return await this.parseHtmlSegment();
          }
        case '{':
          if (this.lookahead('{{')) {
            this.advance(2);
            const result =  this.parseTemplateExpression();
            if (result) {
             
            }
          } else {
            //TODO: add back in control flow
            // const controlFlow = this.parseControlFlow();
            // if (controlFlow) {
            //   return {
            //     type: 'html',
            //     value: [controlFlow] as any,
            //   };
            // }
          }
          break;
        case '-':
          if (this.lookahead('---')) {
            this.advance(3);
            return {
              type: 'javascript',
              value: this.parseJavascript(),
            };
          }
          break;
        default:
          return {
            type: 'javascript',
            value: this.parseText(),
          };
      }
    }
    return undefined;
  }

  public async parse() {
    const tokens: Token[] = [];
    let token;
    while ((token = await this.nextToken())) {
      tokens.push(token as Token);
    }
    return tokens;
  }
}


export type Token =
  | {
      type: 'style';
      value: string;
    }
  | {
      type: 'javascript';
      value: string;
    }
  | {
      type: 'html';
      value: Node[];
    };

export function parseTreaty(input: string) {
  const htmlTokenizer = new HtmlTokenizer(input);
  return htmlTokenizer.parse();
}


export async function parseTreatyAndGroup(input: string) {
    const htmlTokenizer = new HtmlTokenizer(input);
    const tokens: Token[] = await htmlTokenizer.parse();

    const styleTokens: Token[] = [];
    const javascriptTokens: Token[] = [];
    const htmlTokens: Node[][] = [];

    for (const token of tokens) {
        switch (token.type) {
            case 'style':
                styleTokens.push(token);
                break;
            case 'javascript':
                javascriptTokens.push(token);
                break;
            case 'html':
                htmlTokens.push(token.value);
                break;
            default:
                break;
        }
    }

    const styleString = styleTokens.map(token => (token.value as string).replaceAll('\n', '').replaceAll('\t', '') as string);
    const javascriptString = javascriptTokens.map(token => token.value).join('\n');
    const htmlString = htmlTokens.map(content => content.map(c => {
      return c.sourceSpan.toString()
    }).join('\n'));

    return {
      style: styleString,
      javascript: javascriptString,
      html:  htmlString
    }
}

