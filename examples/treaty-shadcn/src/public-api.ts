// == treaty-shadcn public API ==
// The single primary entry that re-exports every component in the library. Each
// component is authored in a different front-end (Treaty JSX `.tsx`, plain React
// `.tsx`, or a `.treaty` SFC) and default-exports its surface; we re-bind each
// default to a PascalCase public name, matching how the examples consume
// `.treaty`/`.tsx` default exports (extensionless specifier, capitalized binding).

export { default as Button } from './button'
export { default as Badge } from './badge'
export { default as Card } from './card'
export { default as Alert } from './alert'
export { default as Input } from './input'
export { default as Switch } from './switch'
