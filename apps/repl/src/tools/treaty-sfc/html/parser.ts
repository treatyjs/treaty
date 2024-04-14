import { Token, Attribute, TokenType } from './tokenizer';

export enum DomNodeType {
  Element,
  Text,
  Comment,
}

export interface DomNode {
  type: DomNodeType;
  element?: ElementNode;
  text?: string;
  comment?: string;
}

export interface ElementNode {
  tagName: string;
  attributes: Attribute[];
  children: DomNode[];
}

export class Parser {
  private tokens: Token[];
  private current: number;

  constructor(tokens: Token[]) {
    this.tokens = tokens;
    this.current = 0;
  }

  public parse(): DomNode[] {
    const nodes: DomNode[] = [];

    while (this.current < this.tokens.length) {
      const token = this.tokens[this.current];

      switch (token.type) {
        case TokenType.Text:
          nodes.push({ type: DomNodeType.Text, text: token.text });
          this.current++;
          break;
        case TokenType.Comment:
          nodes.push({ type: DomNodeType.Comment, comment: token.comment });
          this.current++;
          break;
        case TokenType.StartTag:
        case TokenType.SelfClosingTag:
          const node = this.parseElement();
          if (node) {
            nodes.push(node);
          }
          break;
        default:
          this.current++;
          break;
      }
    }

    return nodes;
  }

  private parseElement(): DomNode | undefined {
    const token = this.tokens[this.current];

    if (token) {
      switch (token.type) {
        case TokenType.StartTag:
        case TokenType.SelfClosingTag:
          this.current++;

          const children: DomNode[] = [];
          while (this.current < this.tokens.length) {
            const currentToken = this.tokens[this.current];

            switch (currentToken.type) {
              case TokenType.EndTag:
                if (currentToken.tagName === token.tagName) {
                  this.current++;
                  return {
                    type: DomNodeType.Element,
                    element: {
                      tagName: token.tagName as string,
                      attributes: token.attributes || [],
                      children,
                    },
                  };
                }
                break;
              case TokenType.Text:
                if (currentToken.text) {
                  children.push({
                    type: DomNodeType.Text,
                    text: currentToken.text,
                  });
                }
                this.current++;
                break;
              default:
                const child = this.parseElement();
                if (child) {
                  children.push(child);
                } else {
                  this.current++;
                }
                break;
            }
          }
          break;
        default:
          this.current++;
          break;
      }
    }

    return undefined;
  }
}
