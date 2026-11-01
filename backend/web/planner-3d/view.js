import * as THREE from '/assets/vendor/three-0.180.0/three.module.min.js';
import {OrbitControls} from '/assets/vendor/three-0.180.0/OrbitControls.js';
import {buildModel,disposeGroup} from './model.js';
import {tileCoverage} from './geometry.js';

export function createViewer(host,callbacks={}){
 const renderer=new THREE.WebGLRenderer({antialias:true,alpha:false,powerPreference:'low-power'});
 renderer.setPixelRatio(Math.min(devicePixelRatio||1,innerWidth<800?1:1.5));renderer.outputColorSpace=THREE.SRGBColorSpace;renderer.toneMapping=THREE.ACESFilmicToneMapping;renderer.toneMappingExposure=1.05;renderer.shadowMap.enabled=true;renderer.shadowMap.type=THREE.PCFSoftShadowMap;
 renderer.domElement.setAttribute('aria-label','Diseño tridimensional del albergue; arrastra para girar y usa la rueda para acercar');renderer.domElement.tabIndex=0;host.append(renderer.domElement);
 const scene=new THREE.Scene();scene.background=new THREE.Color('#e8ede5');const camera=new THREE.PerspectiveCamera(42,1,.1,10000);camera.position.set(100,100,100);
 const controls=new OrbitControls(camera,renderer.domElement);controls.enableDamping=true;controls.dampingFactor=.12;controls.maxPolarAngle=Math.PI*.48;controls.minPolarAngle=.015;controls.screenSpacePanning=true;
 scene.add(new THREE.HemisphereLight('#f4f7fa','#73866a',1.7));
 const sun=new THREE.DirectionalLight('#fff4df',2.3);sun.castShadow=true;sun.shadow.mapSize.set(innerWidth<800?768:1024,innerWidth<800?768:1024);sun.shadow.bias=-.00018;sun.shadow.normalBias=.065;scene.add(sun,sun.target);
 const fill=new THREE.DirectionalLight('#b8d5e2',.5);fill.position.set(-80,40,-70);scene.add(fill);
 let model=null,geometry=null,plan=null,ground=null,tiles=null,frame=null,active=true,visible=true,disposed=false,generation=0,textureGeneration=0,renderedFrames=0,roofsVisible=true,aerialEnabled=false,lastGround=null,selection=null;
 const raycaster=new THREE.Raycaster(),pointer=new THREE.Vector2();let pointerDown=null;
 function invalidate(){if(!disposed&&active&&visible&&!frame)frame=requestAnimationFrame(()=>{frame=null;controls.update();renderer.render(scene,camera);callbacks.compass?.(Math.atan2(camera.position.x-controls.target.x,camera.position.z-controls.target.z)*180/Math.PI);renderedFrames++;});}
 controls.addEventListener('change',invalidate);
 function resize(){if(!active||host.clientWidth<1||host.clientHeight<1)return;const width=host.clientWidth,height=host.clientHeight;camera.aspect=width/height;camera.updateProjectionMatrix();renderer.setSize(width,height,false);invalidate();}
 const observer=new ResizeObserver(resize);observer.observe(host);
 const intersection=new IntersectionObserver(entries=>{visible=entries[0].isIntersecting;if(visible)invalidate();});intersection.observe(host);
 renderer.domElement.addEventListener('webglcontextlost',e=>{e.preventDefault();callbacks.error?.('Se perdió el contexto 3D. Recarga la vista o continúa con el mapa.');});
 function clearTiles(){textureGeneration++;if(tiles){scene.remove(tiles);disposeGroup(tiles);tiles=null;}lastGround=null;if(model)model.surface.visible=true;}
 function clear(){generation++;clearTiles();if(model){scene.remove(model.root);disposeGroup(model.root);model=null;}if(ground){scene.remove(ground);disposeGroup(ground);ground=null;}if(selection){scene.remove(selection);disposeGroup(selection);selection=null;}}
 function frameSite(top=false,detail=false){
  if(!model)return;const housing=model.modules;const e=detail&&housing.length?{minX:Math.min(...housing.map(m=>m.x-m.width/2))-2,maxX:Math.max(...housing.map(m=>m.x+m.width/2))+2,minZ:Math.min(...housing.map(m=>m.z-m.depth/2))-2,maxZ:Math.max(...housing.map(m=>m.z+m.depth/2))+2}:model.extent,span=Math.max(e.maxX-e.minX,e.maxZ-e.minZ,20),center=new THREE.Vector3((e.minX+e.maxX)/2,.6,(e.minZ+e.maxZ)/2),direction=new THREE.Vector3(top?0:.65,top?1:.72,top?.0001:.85).normalize();
  controls.target.copy(center);camera.far=Math.max(span*50,2000);camera.updateProjectionMatrix();
  const corners=[];for(const x of [e.minX,e.maxX])for(const z of [e.minZ,e.maxZ])for(const y of [0,4])corners.push(new THREE.Vector3(x,y,z));
  let low=5,high=span*20;for(let i=0;i<28;i++){const distance=(low+high)/2;camera.position.copy(center).addScaledVector(direction,distance);camera.lookAt(center);camera.updateMatrixWorld();const fits=corners.every(c=>{const p=c.clone().project(camera);return p.z>-1&&p.z<1&&Math.abs(p.x)<.87&&Math.abs(p.y)<.87;});if(fits)high=distance;else low=distance;}
  camera.position.copy(center).addScaledVector(direction,high);controls.minDistance=3;controls.maxDistance=span*20;const damping=controls.enableDamping;controls.enableDamping=false;controls.update();controls.enableDamping=damping;invalidate();
 }
 function refreshAerial(){
  clearTiles();if(!aerialEnabled||!geometry||!model)return;
  const token=textureGeneration,cover=tileCoverage(geometry,9);tiles=new THREE.Group();tiles.name='Imágenes externas de referencia · excluidas del GLB';scene.add(tiles);let loaded=0,errors=0;
  const loader=new THREE.TextureLoader();loader.setCrossOrigin('anonymous');
  for(const tile of cover){const url=`https://services.arcgisonline.com/ArcGIS/rest/services/World_Imagery/MapServer/tile/${tile.z}/${tile.y}/${tile.x}`;
   loader.load(url,texture=>{
    if(disposed||token!==textureGeneration){texture.dispose();return;}texture.colorSpace=THREE.SRGBColorSpace;texture.anisotropy=Math.min(4,renderer.capabilities.getMaxAnisotropy());
    const [west,north]=model.project.point([tile.west,tile.north]),[east,south]=model.project.point([tile.east,tile.south]);const geo=new THREE.PlaneGeometry(east-west,south-north);geo.rotateX(-Math.PI/2);
    const mesh=new THREE.Mesh(geo,new THREE.MeshBasicMaterial({map:texture,toneMapped:false}));mesh.position.set((west+east)/2,-.035,(north+south)/2);tiles.add(mesh);model.surface.visible=false;loaded++;lastGround={requested:cover.length,loaded,errors};callbacks.aerial?.(loaded===cover.length?'Imagen aérea cargada · referencia visual, no en vivo':`Cargando imagen aérea: ${loaded}/${cover.length}`);invalidate();
   },undefined,()=>{if(token!==textureGeneration||disposed)return;errors++;lastGround={requested:cover.length,loaded,errors};callbacks.aerial?.('Parte de la imagen externa no está disponible. Puedes desactivar el suelo satelital.');invalidate();});
  }
 }
 function show(nextGeometry,nextPlan=null){
  clear();geometry=nextGeometry;plan=nextPlan;model=buildModel(geometry,plan);scene.add(model.root);setRoofs(roofsVisible);
  const e=model.extent,span=Math.max(e.maxX-e.minX,e.maxZ-e.minZ,20),center=new THREE.Vector3((e.minX+e.maxX)/2,0,(e.minZ+e.maxZ)/2);
  ground=new THREE.Group();const plane=new THREE.Mesh(new THREE.PlaneGeometry(span*6,span*6),new THREE.MeshStandardMaterial({color:'#dce3d5',roughness:1}));plane.rotation.x=-Math.PI/2;plane.position.set(center.x,-.085,center.z);plane.receiveShadow=true;ground.add(plane);scene.add(ground);
  sun.position.copy(center).add(new THREE.Vector3(span*.7,span*1.3,span*.5));sun.target.position.copy(center);const sc=sun.shadow.camera,half=span*.8+10;sc.left=-half;sc.right=half;sc.top=half;sc.bottom=-half;sc.near=.5;sc.far=span*4+100;sc.updateProjectionMatrix();
  scene.updateMatrixWorld(true);resize();frameSite(false,Boolean(plan));refreshAerial();
  const services=model.services;callbacks.summary?.(plan?`${plan.capacity.proposed_people} plazas propuestas · ${model.modules.length} módulos · ${services.toiletsShown} baños y ${services.tanksShown} depósitos ilustrativos ubicados.`:'Huella del inventario. Calcula una propuesta para ver módulos y dotación.');
  callbacks.details?.(plan?`Cada módulo representa hasta 20 plazas. Depósitos nominales de 1.000 L: ${services.tanksRequested} para representar el agua total del escenario. ${services.placed<services.requested?`Se ubicaron ${services.placed} de ${services.requested} elementos de servicio: falta distribuir el resto. `:''}${model.modules.length>80?'Los interiores se muestran en los primeros 80 módulos. ':''}Suelo plano de referencia; cubiertas, interiores y servicios son un diseño conceptual pendiente de validación.`:'El relieve, los obstáculos y los servicios existentes no se deducen de la imagen.');
  return diagnostics();
 }
 function setRoofs(value){roofsVisible=Boolean(value);model?.meshes.forEach(m=>{if(m.userData.roof)m.visible=roofsVisible;});invalidate();}
 function setActive(value){active=Boolean(value);if(!active&&frame){cancelAnimationFrame(frame);frame=null;}if(active){resize();invalidate();}}
 function selectModule(id){
  if(!model)return;const item=model.modules.find(m=>m.id===id);if(!item)return;
  if(selection){scene.remove(selection);disposeGroup(selection);}selection=new THREE.Group();const coords=[[item.x-item.width/2,.25,item.z-item.depth/2],[item.x+item.width/2,.25,item.z-item.depth/2],[item.x+item.width/2,.25,item.z+item.depth/2],[item.x-item.width/2,.25,item.z+item.depth/2],[item.x-item.width/2,.25,item.z-item.depth/2]];const geo=new THREE.BufferGeometry().setFromPoints(coords.map(p=>new THREE.Vector3(...p)));selection.add(new THREE.Line(geo,new THREE.LineBasicMaterial({color:'#f2ad36',depthTest:false})));scene.add(selection);callbacks.selection?.(`Módulo ${item.id} · ${item.people} plazas propuestas · ${Math.round(item.width)} × ${Math.round(item.depth)} m. Ocupación real sin confirmar.`);invalidate();
 }
 renderer.domElement.addEventListener('pointerdown',e=>{pointerDown=[e.clientX,e.clientY];});
 renderer.domElement.addEventListener('pointerup',e=>{if(!model||!pointerDown||Math.hypot(e.clientX-pointerDown[0],e.clientY-pointerDown[1])>5)return;const b=renderer.domElement.getBoundingClientRect();pointer.set((e.clientX-b.left)/b.width*2-1,-(e.clientY-b.top)/b.height*2+1);raycaster.setFromCamera(pointer,camera);const hit=raycaster.intersectObjects(model.meshes.filter(m=>m.visible),false).find(h=>h.object.userData.moduleIds[h.instanceId]!=null);if(hit)selectModule(hit.object.userData.moduleIds[hit.instanceId]);});
 function rotate(direction){const offset=camera.position.clone().sub(controls.target);offset.applyAxisAngle(new THREE.Vector3(0,1,0),direction*Math.PI/8);camera.position.copy(controls.target).add(offset);controls.update();invalidate();}
 function download(blob,name){const url=URL.createObjectURL(blob),link=document.createElement('a');link.href=url;link.download=name;link.click();setTimeout(()=>URL.revokeObjectURL(url),1000);}
 function screenshot(){if(!model)return;renderer.render(scene,camera);const canvas=document.createElement('canvas');canvas.width=renderer.domElement.width;canvas.height=renderer.domElement.height+80;const context=canvas.getContext('2d');context.drawImage(renderer.domElement,0,0);context.fillStyle='#12372f';context.fillRect(0,canvas.height-80,canvas.width,80);context.fillStyle='#f1f5e8';context.font='14px sans-serif';context.fillText('Territorio Preparado · Diseño conceptual sujeto a revisión',18,canvas.height-51);context.font='11px sans-serif';context.fillText(aerialEnabled?'Imágenes: Esri, Vantor, Earthstar Geographics y la comunidad GIS · fecha y resolución variables':'Superficie plana de referencia · no representa ocupación ni instalaciones confirmadas',18,canvas.height-25);canvas.toBlob(blob=>{if(blob)download(blob,'albergue-'+(plan?.space.id||'huella')+'.png');},'image/png');}
 async function exportGLB(){if(!plan||!model)throw Error('Calcula primero la propuesta.');const {GLTFExporter}=await import('/assets/vendor/three-0.180.0/GLTFExporter.js');const hidden=[];if(!model.surface.visible){hidden.push(model.surface);model.surface.visible=true;}model.root.traverse(o=>{if(o.isInstancedMesh&&o.userData.roof&&!o.visible){hidden.push(o);o.visible=true;}});try{const binary=await new GLTFExporter().parseAsync(model.root,{binary:true,onlyVisible:true});download(new Blob([binary],{type:'model/gltf-binary'}),'albergue-'+plan.space.id+'.glb');return {bytes:binary.byteLength,modules:model.modules.length};}finally{hidden.forEach(o=>o.visible=false);invalidate();}}
 function diagnostics(){return {ready:Boolean(model),engine:'Three.js '+THREE.REVISION,moduleCount:model?.modules.length||0,interiorModules:model?.interiorModules||0,roofVisible:roofsVisible,services:model?.services||null,meshes:model?.meshes.length||0,renderedFrames,memory:{...renderer.info.memory},aerial:lastGround,active,planEngine:plan?.engine||null};}
 function dispose(){if(disposed)return;clear();disposed=true;if(frame)cancelAnimationFrame(frame);observer.disconnect();intersection.disconnect();controls.dispose();scene.traverse(o=>{if(o.isLight)o.dispose?.();});renderer.dispose();renderer.domElement.remove();}
 window.addEventListener('pagehide',dispose,{once:true});
 resize();return {show,setRoofs,setActive,fit:()=>frameSite(false),focusHousing:()=>frameSite(false,true),top:()=>frameSite(true),rotate,setAerial:value=>{aerialEnabled=Boolean(value);refreshAerial();},screenshot,exportGLB,diagnostics,reset:()=>{clear();geometry=null;plan=null;invalidate();},dispose};
}
