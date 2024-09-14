export enum TokenType {
    StartTag,
    EndTag,
    SelfClosingTag,
    Text,
    Comment,
  }
  
  export interface Attribute {
    name: string;
    value: string;
  }
  
  export interface Token {
    type: TokenType;
    tagName?: string;
    attributes?: Attribute[];
    text?: string;
    comment?: string;
  }
  
  export class HtmlTokenizer {
    private input: string;
    private pos: number;
  
    constructor(input: string) {
      this.input = input;
      this.pos = 0;
    }
  
    public nextToken(): Token | null {
      this.skipWhitespace();
      if (this.pos >= this.input.length) {
        return null;
      }
      switch (this.input[this.pos]) {
        case "<":
          if (this.startsWith("<!--")) {
            return this.consumeComment();
          } else if (this.startsWith("</")) {
            return this.consumeEndTag();
          } else {
            return this.consumeStartOrSelfClosingTag();
          }
        default:
          return this.consumeText();
      }
    }
  
    private skipWhitespace(): void {
      while (this.pos < this.input.length && /\s/.test(this.input[this.pos])) {
        this.pos++;
      }
    }
  
    private startsWith(start: string): boolean {
      return this.input.slice(this.pos).startsWith(start);
    }
  
    private consumeWhile(condition: (char: string) => boolean): string {
      const start = this.pos;
      while (this.pos < this.input.length && condition(this.input[this.pos])) {
        this.pos++;
      }
      return this.input.slice(start, this.pos);
    }
  
    private consumeComment(): Token | null {
      this.pos += 4;
  
      const startPos = this.pos;
      while (!this.startsWith("-->") && this.pos < this.input.length) {
        this.pos++;
      }
      const comment = this.input.slice(startPos, this.pos);
  
      if (this.startsWith("-->")) {
        this.pos += 3;
        return { type: TokenType.Comment, comment };
      } else {
        return null;
      }
    }
  
    private consumeEndTag(): Token | null {
      this.pos += 2;
      const tagName = this.consumeWhile((char) => char !== ">");
      this.pos++;
      return { type: TokenType.EndTag, tagName };
    }
  
    private consumeStartOrSelfClosingTag(): Token | null {
      this.pos++;
      const tagName = this.consumeWhile((char) => !/\s|>|\/$/.test(char));
      const attributes = this.consumeAttributes();
      if (this.startsWith("/>")) {
        this.pos += 2;
        return { type: TokenType.SelfClosingTag, tagName, attributes };
      } else {
        this.pos++;
        return { type: TokenType.StartTag, tagName, attributes };
      }
    }
  
    private consumeAttributes(): Attribute[] {
      const attributes: Attribute[] = [];
      while (
        this.pos < this.input.length &&
        !this.startsWith(">") &&
        !this.startsWith("/>")
      ) {
        this.skipWhitespace();
        if (this.startsWith(">") || this.startsWith("/>")) {
          break;
        }
        const name = this.consumeWhile(
          (char) => char !== "=" && !/\s/.test(char)
        );
        this.skipWhitespace();
        if (
          this.pos < this.input.length &&
          this.input[this.pos] === "=" &&
          this.pos + 1 < this.input.length
        ) {
          this.pos++; // Skip '='
          this.skipWhitespace();
          const quote = this.input[this.pos];
          if (quote === '"' || quote === "'") {
            this.pos++;
            const value = this.consumeWhile((char) => char !== quote);
            this.pos++;
            attributes.push({ name, value });
          }
        }
      }
      return attributes;
    }
  
    private consumeText(): Token {
      const text = this.consumeWhile((char) => char !== "<");
      return { type: TokenType.Text, text: text.trim() };
    }
  }