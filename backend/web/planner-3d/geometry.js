// Coordinates and conservative checks shared by the model and aerial context.
export const EARTH_RADIUS=6371008.8;
export function polygons(geometry){return geometry.type==='Polygon'?[geometry.coordinates]:geometry.type==='MultiPolygon'?geometry.coordinates:[];}
export function projection(geometry){
 const points=polygons(geometry).flatMap(p=>p[0]),lons=points.map(p=>p[0]),lats=points.map(p=>p[1]);
 const lon=(Math.min(...lons)+Math.max(...lons))/2,lat=(Math.min(...lats)+Math.max(...lats))/2,kx=EARTH_RADIUS*Math.cos(lat*Math.PI/180)*Math.PI/180,ky=EARTH_RADIUS*Math.PI/180;
 return {lon,lat,point:([x,y])=>[(x-lon)*kx,-(y-lat)*ky],unproject:([x,z])=>[lon+x/kx,lat-z/ky]};
}
export function bounds(geometry,project){const points=polygons(geometry).flatMap(p=>p[0]).map(project.point);return {minX:Math.min(...points.map(p=>p[0])),maxX:Math.max(...points.map(p=>p[0])),minZ:Math.min(...points.map(p=>p[1])),maxZ:Math.max(...points.map(p=>p[1]))};}
export function projectedPolygons(geometry,project){return polygons(geometry).map(p=>p.map(r=>r.map(project.point)));}
function insideRing([x,y],ring){let inside=false;for(let i=0,j=ring.length-1;i<ring.length;j=i++){const a=ring[i],b=ring[j];if((a[1]>y)!==(b[1]>y)&&x<(b[0]-a[0])*(y-a[1])/(b[1]-a[1])+a[0])inside=!inside;}return inside;}
export function containsPoint(point,polys){return polys.some(p=>insideRing(point,p[0])&&!p.slice(1).some(h=>insideRing(point,h)));}
const orientation=(a,b,c)=>(b[0]-a[0])*(c[1]-a[1])-(b[1]-a[1])*(c[0]-a[0]);
function crosses(a,b,c,d){return orientation(a,b,c)*orientation(a,b,d)<-1e-10&&orientation(c,d,a)*orientation(c,d,b)<-1e-10;}
export function containsRectangle(x,z,width,depth,polys){
 const corners=[[x-width/2,z-depth/2],[x+width/2,z-depth/2],[x+width/2,z+depth/2],[x-width/2,z+depth/2]];
 if(!corners.every(p=>containsPoint(p,polys)))return false;
 for(const poly of polys)for(const ring of poly){
   // A complete hole inside the box also excludes it, without edge crossings.
   if(ring.some(p=>p[0]>x-width/2&&p[0]<x+width/2&&p[1]>z-depth/2&&p[1]<z+depth/2))return false;
   for(let n=1;n<ring.length;n++)for(let c=0;c<4;c++)if(crosses(ring[n-1],ring[n],corners[c],corners[(c+1)%4]))return false;
 }
 return true;
}
export function servicePositions(geometry,project,count){
 if(count<=0)return [];
 const b=bounds(geometry,project),polys=projectedPolygons(geometry,project),out=[];let scanned=0;
 for(let z=b.minZ+1.4;z<=b.maxZ-1.4;z+=2.8)for(let x=b.minX+1.4;x<=b.maxX-1.4;x+=2.8){
  if(++scanned>12000)return out;
  if(containsRectangle(x,z,2.3,2.3,polys))out.push([x,z]);
  if(out.length>=count)return out;
 }
 return out;
}
export function tileCoverage(geometry,maxTiles=9){
 const points=polygons(geometry).flatMap(p=>p[0]),west=Math.min(...points.map(p=>p[0])),east=Math.max(...points.map(p=>p[0])),south=Math.min(...points.map(p=>p[1])),north=Math.max(...points.map(p=>p[1]));
 const ty=(lat,n)=>(1-Math.asinh(Math.tan(lat*Math.PI/180))/Math.PI)/2*n;
 for(let z=19;z>=8;z--){const n=2**z,x0=Math.floor((west+180)/360*n),x1=Math.floor((east+180)/360*n),y0=Math.floor(ty(north,n)),y1=Math.floor(ty(south,n));if((x1-x0+1)*(y1-y0+1)>maxTiles)continue;
  const lat=y=>Math.atan(Math.sinh(Math.PI*(1-2*y/n)))*180/Math.PI,tiles=[];
  for(let y=y0;y<=y1;y++)for(let x=x0;x<=x1;x++)tiles.push({z,x,y,west:x/n*360-180,east:(x+1)/n*360-180,north:lat(y),south:lat(y+1)});
  return tiles;
 }
 return [];
}
