import { Token, ControlFlowKind, DeferKind, TokenType } from './token';

enum LexerState {
    Default,
    JavaScript,
    HTML,
    CSS,
    TemplateExpression,
    ControlFlow,
}

class Lexer {
    private input: string;
    private pos: number;
    private currentChar: string | null;
    private state: LexerState;
    private stateStack: LexerState[];

    constructor(input: string) {
        this.input = input;
        this.pos = 0;
        this.currentChar = input[0] || null;
        this.state = LexerState.Default;
        this.stateStack = [];
    }

    nextToken(): Token | null {
        this.consumeWhitespace();

        switch (this.state) {
            case LexerState.Default:
                return this.lexDefaultState();
            case LexerState.JavaScript:
                return this.parseJavaScript();
            case LexerState.HTML:
                return this.parseHTML();
            case LexerState.CSS:
                return this.parseStyle();
            case LexerState.TemplateExpression:
                return this.parseTemplateExpression();
            case LexerState.ControlFlow:
                return this.parseControlFlow();
        }
    }

    private advance(): void {
        this.pos++;
        this.currentChar = this.pos < this.input.length ? this.input[this.pos] : null;
    }

    private consumeWhile(condition: (ch: string) => boolean): string {
        let result = '';
        while (this.currentChar !== null && condition(this.currentChar)) {
            result += this.currentChar;
            this.advance();
        }
        return result;
    }

    private consumeWhitespace(): void {
        this.consumeWhile(ch => /\s/.test(ch));
    }

    private lexDefaultState(): Token | null {
        if (this.currentChar === null) return null;

        if (this.currentChar === '<') {
            if (this.startsWith('<style>')) {
                this.advanceBy('<style>'.length);
                this.pushState(LexerState.CSS);
                return this.parseStyle();
            } else {
                this.pushState(LexerState.HTML);
                return this.parseHTML();
            }
        } else if (this.currentChar === '{' && this.startsWith('{{')) {
            this.advanceBy(2);
            this.pushState(LexerState.TemplateExpression);
            return this.parseTemplateExpression();
        } else if (this.currentChar === '@') {
            return this.parseControlFlow();
        } else {
            this.pushState(LexerState.JavaScript);
            return this.parseJavaScript();
        }
    }

    private parseJavaScript(): Token {
        const startPos = this.pos;
        while (this.currentChar !== null) {
            if (this.currentChar === "'" || this.currentChar === '"' || this.currentChar === '`') {
                this.consumeString(this.currentChar);
            } else if (this.startsWith('//')) {
                this.consumeLineComment();
            } else if (this.startsWith('/*')) {
                this.consumeBlockComment();
            } else if (/[\n\r\f;]/.test(this.currentChar)) {
                this.advance();
                break;
            } else if ((this.startsWith('<style>') || this.startsWith('</'))) {
                break;
            } else if (this.startsWith('{{')) {
                break;
            } else if (this.currentChar === '@') {
                this.pushState(LexerState.ControlFlow);
                break;
            } else {
                this.advance();
            }
        }

        const endPos = this.pos;
        const value = this.input.slice(startPos, endPos);
        this.popState();
        return Token.new({ type: TokenType.JavaScript, value }, startPos, endPos);
    }

    private parseStyle(): Token {
        const startPos = this.pos;
        while (this.currentChar !== null) {
            if (this.startsWith('</style>')) {
                break;
            }
            if (this.startsWith('/*')) {
                this.consumeBlockComment();
            } else if (this.currentChar === '{' || this.currentChar === '}') {
                this.advance();
            } else {
                this.advance();
            }
        }

        const endPos = this.pos;
        const value = this.input.slice(startPos, endPos);

        if (this.startsWith('</style>')) {
            this.advanceBy('</style>'.length);
        }

        this.popState();
        return Token.new({ type: TokenType.Style, value }, startPos, endPos);
    }

    private parseHTML(): Token {
        const startPos = this.pos;
        const tagStack: string[] = [];
        while (this.currentChar !== null) {
            this.consumeWhitespace();
            if (this.currentChar === '<') {
                if (this.startsWith('<!--')) {
                    this.consumeHTMLComment();
                } else if (this.startsWith('</')) {
                    this.advanceBy(2);
                    const tagName = this.consumeTagName();
                    const expectedTag = tagStack.pop();
                    if (expectedTag !== undefined && tagName !== expectedTag) {
                        // Handle mismatched tag (optional)
                    }
                    this.consumeUntil('>');
                    this.advance();
                    if (tagStack.length === 0) {
                        break;
                    }
                } else if (this.startsWith('<')) {
                    this.advance();
                    const tagName = this.consumeTagName();
                    tagStack.push(tagName);
                    this.consumeAttributes();
                    // this.advance();
                } else {
                    this.advance();
                }
            } else if (this.currentChar === '{' && this.startsWith('{{')) {
                break;
            } else {
                this.advance();
            }
        }

        const endPos = this.pos;
        const value = this.input.slice(startPos, endPos);
        this.popState();
        return Token.new({ type: TokenType.HTML, value }, startPos, endPos);
    }

    private parseTemplateExpression(): Token {
        const startPos = this.pos;
        let braceCount = 0;

        while (this.currentChar !== null) {
            if (this.currentChar === '{') {
                braceCount++;
            } else if (this.currentChar === '}') {
                braceCount--;
                if (braceCount === -2) {
                    break;
                }
            } else if (this.currentChar === "'" || this.currentChar === '"' || this.currentChar === '`') {
                this.consumeString(this.currentChar);
                continue;
            }
            this.advance();
        }

        if (this.startsWith('}}')) {
            this.advanceBy(2);
        }

        const endPos = this.pos;
        const value = this.input.slice(startPos, endPos);
        this.popState();
        return Token.new({ type: TokenType.TemplateExpression, value: value.trim() }, startPos, endPos);
    }

    private parseControlFlow(): Token {
        const startPos = this.pos;

        if (this.startsWith('@if')) {
            this.advanceBy('@if'.length);
            return Token.new({ type: TokenType.ControlFlow, kind: ControlFlowKind.If }, startPos, this.pos);
        } else if (this.startsWith('@else if')) {
            this.advanceBy('@else if'.length);
            return Token.new({ type: TokenType.ControlFlow, kind: ControlFlowKind.ElseIf }, startPos, this.pos);
        } else if (this.startsWith('@else')) {
            this.advanceBy('@else'.length);
            return Token.new({ type: TokenType.ControlFlow, kind: ControlFlowKind.Else }, startPos, this.pos);
        } else if (this.startsWith('@for')) {
            this.advanceBy('@for'.length);
            return Token.new({ type: TokenType.ControlFlow, kind: ControlFlowKind.For }, startPos, this.pos);
        } else if (this.startsWith('@empty')) {
            this.advanceBy('@empty'.length);
            return Token.new({ type: TokenType.ControlFlow, kind: ControlFlowKind.Empty }, startPos, this.pos);
        } else if (this.startsWith('@switch')) {
            this.advanceBy('@switch'.length);
            return Token.new({ type: TokenType.ControlFlow, kind: ControlFlowKind.Switch }, startPos, this.pos);
        } else if (this.startsWith('@case')) {
            this.advanceBy('@case'.length);
            return Token.new({ type: TokenType.ControlFlow, kind: ControlFlowKind.Case }, startPos, this.pos);
        } else if (this.startsWith('@default')) {
            this.advanceBy('@default'.length);
            return Token.new({ type: TokenType.ControlFlow, kind: ControlFlowKind.Default }, startPos, this.pos);
        } else if (this.startsWith('@defer')) {
            this.advanceBy('@defer'.length);
            return Token.new({ type: TokenType.Defer, kind: DeferKind.Defer }, startPos, this.pos);
        } else if (this.startsWith('@placeholder')) {
            this.advanceBy('@placeholder'.length);
            return Token.new({ type: TokenType.Defer, kind: DeferKind.Placeholder }, startPos, this.pos);
        } else if (this.startsWith('@loading')) {
            this.advanceBy('@loading'.length);
            return Token.new({ type: TokenType.Defer, kind: DeferKind.Loading }, startPos, this.pos);
        } else if (this.startsWith('@error')) {
            this.advanceBy('@error'.length);
            return Token.new({ type: TokenType.Defer, kind: DeferKind.Error }, startPos, this.pos);
        } else {
            this.state = LexerState.JavaScript;
            this.advance();
            return this.parseJavaScript();
        }
    }

    private startsWith(s: string): boolean {
        return this.input.slice(this.pos).startsWith(s);
    }

    private advanceBy(n: number): void {
        for (let i = 0; i < n; i++) {
            this.advance();
        }
    }

    private consumeString(delimiter: string): void {
        this.advance();
        while (this.currentChar !== null) {
            if (this.currentChar === '\\') {
                this.advance();
                this.advance();
            } else if (this.currentChar === delimiter) {
                this.advance();
                break;
            } else {
                this.advance();
            }
        }
    }

    private consumeLineComment(): void {
        while (this.currentChar !== null && this.currentChar !== '\n') {
            this.advance();
        }
    }

    private consumeBlockComment(): void {
        this.advanceBy(2);
        while (this.currentChar !== null) {
            if (this.startsWith('*/')) {
                this.advanceBy(2);
                break;
            }
            this.advance();
        }
    }

    private consumeHTMLComment(): void {
        this.advanceBy('<!--'.length);
        while (this.currentChar !== null) {
            if (this.startsWith('-->')) {
                this.advanceBy('-->'.length);
                break;
            }
            this.advance();
        }
    }

    private consumeTagName(): string {
        return this.consumeWhile(ch => /[a-zA-Z0-9]/.test(ch));
    }

    private consumeAttributes(): boolean {
        let selfClosing = false;
        while (this.currentChar !== null) {
            if (this.currentChar === '>') {
                this.advance();
                break;
            } else if (this.currentChar === '/' && this.peek() === '>') {
                this.advanceBy(2);
                selfClosing = true;
                break;
            } else if (this.currentChar === "'" || this.currentChar === '"' || this.currentChar === '`') {
                this.consumeString(this.currentChar);
            } else {
                this.advance();
            }
        }
        return selfClosing;
    }

    private consumeUntil(target: string): void {
        while (this.currentChar !== null && this.currentChar !== target) {
            this.advance();
        }
    }

    private peek(): string | null {
        return this.pos + 1 < this.input.length ? this.input[this.pos + 1] : null;
    }

    private pushState(state: LexerState): void {
        this.stateStack.push(this.state);
        this.state = state;
    }

    private popState(): void {
        const state = this.stateStack.pop();
        if (state !== undefined) {
            this.state = state;
        }
    }
}

export { Lexer, LexerState };