export enum TokenType {
    JavaScript = 'JavaScript',
    HTML = 'HTML',
    Style = 'Style',
    TemplateExpression = 'TemplateExpression',
    Defer = 'Defer',
    ControlFlow = 'ControlFlow',
    Eof = 'Eof'
}

export enum ControlFlowKind {
    If,
    ElseIf,
    Else,
    For,
    Empty,
    Switch,
    Case,
    Default,
}

export enum DeferKind {
    Defer,
    Placeholder,
    Loading,
    Error,
}

export type TokenKind =
    | { type: TokenType.JavaScript, value: string }
    | { type: TokenType.HTML, value: string }
    | { type: TokenType.Style, value: string }
    | { type: TokenType.TemplateExpression, value: string }
    | { type: TokenType.Defer, kind: DeferKind }
    | { type: TokenType.ControlFlow, kind: ControlFlowKind }
    | { type: TokenType.Eof };

export class Token {
    constructor(
        public kind: TokenKind,
        public start: number,
        public end: number
    ) {}

    static new(kind: TokenKind, start: number, end: number): Token {
        return new Token(kind, start, end);
    }
}