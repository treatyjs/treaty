export type ProductCategory = 'hardware' | 'software' | 'service';

export interface Product {
  readonly id: number;
  readonly name: string;
  readonly category: ProductCategory;
  /** Price in integer cents to keep the currency pipe lossless. */
  readonly priceCents: number;
  readonly inStock: boolean;
}
