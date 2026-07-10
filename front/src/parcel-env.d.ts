// Parcel 固有の import 形式に対する型宣言
// https://parceljs.org/features/dependency-resolution/#url-schemes
declare module "url:*" {
  const url: string;
  export default url;
}

declare module "*.css";
