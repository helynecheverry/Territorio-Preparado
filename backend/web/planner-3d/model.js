import * as THREE from '/assets/vendor/three-0.180.0/three.module.min.js';
import {projection,polygons,bounds,servicePositions} from './geometry.js';

const palette={canvas:'#e8dcb9',canvasSide:'#d7c89e',steel:'#49675c',floor:'#b9b5a3',bed:'#467d85',pillow:'#f3efe3',toilet:'#3c7e9a',door:'#224b61',tank:'#d8e1d4',water:'#478eaa',services:'#d2aa56',exclusion:'#c65b50'};
function shapeGeometry(geometry,project){
 const shapes=polygons(geometry).map(poly=>{const shape=new THREE.Shape();poly.forEach((ring,index)=>{const path=index?new THREE.Path():shape;ring.forEach((p,i)=>{const [x,z]=project.point(p);if(i===0)path.moveTo(x,-z);else path.lineTo(x,-z);});if(index)shape.holes.push(path);});return shape;});
 const out=new THREE.ShapeGeometry(shapes);out.rotateX(-Math.PI/2);out.computeBoundingSphere();return out;
}
function lineGeometry(geometry,project,height){const values=[];for(const poly of polygons(geometry))for(const ring of poly)for(let i=1;i<ring.length;i++){for(const p of [ring[i-1],ring[i]]){const [x,z]=project.point(p);values.push(x,height,z);}}const out=new THREE.BufferGeometry();out.setAttribute('position',new THREE.Float32BufferAttribute(values,3));out.computeBoundingSphere();return out;}
export function disposeGroup(group){if(!group)return;const geometries=new Set(),materials=new Set(),textures=new Set();group.traverse(o=>{if(o.isInstancedMesh)o.dispose();if(o.geometry)geometries.add(o.geometry);for(const m of (Array.isArray(o.material)?o.material:[o.material]))if(m){materials.add(m);for(const value of Object.values(m))if(value?.isTexture)textures.add(value);}});textures.forEach(t=>t.dispose());geometries.forEach(g=>g.dispose());materials.forEach(m=>m.dispose());group.clear();}
class Parts {
 constructor(root){this.root=root;this.batches=new Map();this.dummy=new THREE.Object3D();this.unitBox=new THREE.BoxGeometry(1,1,1);this.cylinder=new THREE.CylinderGeometry(1,1,1,16);}
 add(kind,color,position,scale,rotation=[0,0,0],module=null,roof=false){const key=`${kind}:${color}:${roof}`;let batch=this.batches.get(key);if(!batch){batch={kind,color,roof,items:[]};this.batches.set(key,batch);}batch.items.push({position,scale,rotation,module});}
 build(){const result=[];for(const b of this.batches.values()){const material=new THREE.MeshStandardMaterial({color:b.color,roughness:.82,metalness:b.color===palette.steel?.15:0});const mesh=new THREE.InstancedMesh(b.kind==='cylinder'?this.cylinder:this.unitBox,material,b.items.length);mesh.name=b.roof?'Cubiertas de alojamiento':'Elementos de albergue';mesh.userData.roof=b.roof;mesh.userData.moduleIds=b.items.map(i=>i.module);mesh.userData.elementCount=b.items.length;for(let i=0;i<b.items.length;i++){const item=b.items[i];this.dummy.position.set(...item.position);this.dummy.scale.set(...item.scale);this.dummy.rotation.set(...item.rotation);this.dummy.updateMatrix();mesh.setMatrixAt(i,this.dummy.matrix);}mesh.instanceMatrix.needsUpdate=true;mesh.computeBoundingBox();mesh.computeBoundingSphere();mesh.castShadow=true;mesh.receiveShadow=true;this.root.add(mesh);result.push(mesh);}if(![...this.batches.values()].some(b=>b.kind==='cylinder'))this.cylinder.dispose();if(![...this.batches.values()].some(b=>b.kind!=='cylinder'))this.unitBox.dispose();return result;}
}
export function buildModel(geometry,plan=null){
 const project=projection(geometry),extent=bounds(geometry,project),root=new THREE.Group();root.name='Propuesta de Territorio Preparado';root.userData={status:'conceptual_requires_review',space_id:plan?.space.id||null,engine:plan?.engine||null};
 const surface=new THREE.Mesh(shapeGeometry(geometry,project),new THREE.MeshStandardMaterial({color:'#789576',roughness:1,side:THREE.DoubleSide}));surface.name='Huella inventariada · terreno plano de referencia';surface.receiveShadow=true;root.add(surface);
 const boundary=new THREE.LineSegments(lineGeometry(geometry,project,.055),new THREE.LineBasicMaterial({color:'#28699b'}));root.add(boundary);
 if(!plan)return {root,project,extent,meshes:[],modules:[],services:{requested:0,placed:0},surface,interiorModules:0};
 const parts=new Parts(root),modules=plan.layout.features.filter(f=>f.properties.role==='shelter_module'),centers=[];
 let reserve=null;
 for(const f of plan.layout.features){if(f.properties.role==='services_reserve'||f.properties.role==='excluded'){const mesh=new THREE.Mesh(shapeGeometry(f.geometry,project),new THREE.MeshStandardMaterial({color:f.properties.role==='excluded'?palette.exclusion:palette.services,roughness:1,side:THREE.DoubleSide}));mesh.position.y=.025;mesh.receiveShadow=true;mesh.name=f.properties.role==='excluded'?'Exclusión declarada':'Reserva de servicios';root.add(mesh);if(f.properties.role==='services_reserve')reserve=f.geometry;}}
 modules.forEach((f,index)=>{
  const b=bounds(f.geometry,project),x=(b.minX+b.maxX)/2,z=(b.minZ+b.maxZ)/2,width=b.maxX-b.minX,depth=b.maxZ-b.minZ,yaw=width<depth?Math.PI/2:0;
  const scaleX=(yaw?depth:width)/10,scaleZ=(yaw?width:depth)/7;
  centers.push({id:f.properties.module,people:f.properties.people,x,z,width,depth});
  const add=(color,lx,y,lz,sx,sy,sz,rz=0,roof=false,kind='box')=>{const px=lx*scaleX,pz=lz*scaleZ;parts.add(kind,color,[x+px*Math.cos(yaw)+pz*Math.sin(yaw),y,z-px*Math.sin(yaw)+pz*Math.cos(yaw)],[sx*scaleX,sy,sz*scaleZ],[0,yaw,rz],f.properties.module,roof);};
  add(palette.floor,0,.105,0,10,.19,7);
  const slope=Math.atan(.9/5),roofWidth=Math.sqrt(25+.81)+.28;
  add(palette.canvas,-2.5,3.25,0,roofWidth,.09,7.5,slope,true);add(palette.canvas,2.5,3.25,0,roofWidth,.09,7.5,-slope,true);
  for(const lx of [-4.8,0,4.8])for(const lz of [-3.25,3.25])add(palette.steel,lx,1.48,lz,.09,2.75,.09);
  for(const lz of [-3.25,3.25])add(palette.steel,0,2.84,lz,9.7,.10,.10);
  add(palette.steel,0,3.67,0,.09,.10,7.1);
  // End gables, restrained fabric side walls, open entrances.
  add(palette.canvasSide,-4.85,1.0,0,.06,1.65,6.5);add(palette.canvasSide,4.85,1.0,0,.06,1.65,6.5);
  if(index<80){for(let n=0;n<f.properties.people;n++){const lx=[-3.6,-1.5,1.5,3.6][n%4],lz=-2.4+Math.floor(n/4)*1.2;add(palette.bed,lx,.29,lz,1.8,.14,.76);add(palette.pillow,lx-.61,.39,lz,.42,.12,.64);} }
 });
 const toilets=plan.requirements_for_proposal.toilets,tanks=Math.ceil(plan.requirements_for_proposal.water_liters_total/1000),requested=toilets+tanks;
 const places=reserve?servicePositions(reserve,project,Math.min(requested,100)):[];
 places.forEach(([x,z],index)=>{if(index<toilets){parts.add('box',palette.toilet,[x,1.22,z],[1.2,2.35,1.2]);parts.add('box',palette.door,[x,1.18,z+.611],[.88,2.03,.03]);parts.add('box',palette.canvas,[x,2.45,z],[1.35,.1,1.35]);parts.add('box','#d9e6df',[x,1.87,z+.635],[.22,.25,.02]);}else{parts.add('box',palette.steel,[x,.13,z],[1.35,.25,1.35]);parts.add('cylinder',palette.tank,[x,.94,z],[.5,1.3,.5]);parts.add('cylinder',palette.steel,[x,1.64,z],[.53,.09,.53]);parts.add('box',palette.water,[x+.49,.55,z],[.20,.1,.1]);}});
 const meshes=parts.build();root.updateMatrixWorld(true);
 return {root,project,extent,meshes,modules:centers,services:{requested,placed:places.length,toiletsShown:Math.min(toilets,places.length),tanksShown:Math.max(0,places.length-toilets),tanksRequested:tanks},surface,interiorModules:Math.min(80,modules.length)};
}
