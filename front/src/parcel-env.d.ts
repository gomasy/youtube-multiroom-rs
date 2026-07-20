// Type declarations for Parcel-specific import schemes
// https://parceljs.org/features/dependency-resolution/#url-schemes
declare module "url:*" {
  const url: string;
  export default url;
}

declare module "*.css";
