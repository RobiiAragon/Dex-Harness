// 100% Offline Pure-JS Binary glTF 2.0 (.glb) Parser & Interactive WebGL2 Viewer
(function (global) {
  'use strict';

  const Mat4 = {
    identity() {
      const m = new Float32Array(16);
      m[0] = 1; m[5] = 1; m[10] = 1; m[15] = 1;
      return m;
    },
    multiply(a, b) {
      const out = new Float32Array(16);
      for (let c = 0; c < 4; c++) {
        for (let r = 0; r < 4; r++) {
          out[c * 4 + r] =
            a[0 * 4 + r] * b[c * 4 + 0] +
            a[1 * 4 + r] * b[c * 4 + 1] +
            a[2 * 4 + r] * b[c * 4 + 2] +
            a[3 * 4 + r] * b[c * 4 + 3];
        }
      }
      return out;
    },
    perspective(fovyRad, aspect, near, far) {
      const f = 1.0 / Math.tan(fovyRad / 2);
      const nf = 1.0 / (near - far);
      const m = new Float32Array(16);
      m[0] = f / Math.max(aspect, 0.01);
      m[5] = f;
      m[10] = (far + near) * nf;
      m[11] = -1;
      m[14] = (2 * far * near) * nf;
      return m;
    },
    lookAt(eye, target, up) {
      let zx = eye[0] - target[0], zy = eye[1] - target[1], zz = eye[2] - target[2];
      let zLen = Math.hypot(zx, zy, zz) || 1;
      zx /= zLen; zy /= zLen; zz /= zLen;

      let xx = up[1] * zz - up[2] * zy;
      let xy = up[2] * zx - up[0] * zz;
      let xz = up[0] * zy - up[1] * zx;
      let xLen = Math.hypot(xx, xy, xz) || 1;
      xx /= xLen; xy /= xLen; xz /= xLen;

      const yx = zy * xz - zz * xy;
      const yy = zz * xx - zx * xz;
      const yz = zx * xy - zy * xx;

      const m = new Float32Array(16);
      m[0] = xx; m[1] = yx; m[2] = zx; m[3] = 0;
      m[4] = xy; m[5] = yy; m[6] = zy; m[7] = 0;
      m[8] = xz; m[9] = yz; m[10] = zz; m[11] = 0;
      m[12] = -(xx * eye[0] + xy * eye[1] + xz * eye[2]);
      m[13] = -(yx * eye[0] + yy * eye[1] + yz * eye[2]);
      m[14] = -(zx * eye[0] + zy * eye[1] + zz * eye[2]);
      m[15] = 1;
      return m;
    },
    fromTRS(t, q, s) {
      const tx = t ? t[0] : 0, ty = t ? t[1] : 0, tz = t ? t[2] : 0;
      const qx = q ? q[0] : 0, qy = q ? q[1] : 0, qz = q ? q[2] : 0, qw = q ? q[3] : 1;
      const sx = s ? s[0] : 1, sy = s ? s[1] : 1, sz = s ? s[2] : 1;

      const x2 = qx + qx, y2 = qy + qy, z2 = qz + qz;
      const xx = qx * x2, xy = qx * y2, xz = qx * z2;
      const yy = qy * y2, yz = qy * z2, zz = qz * z2;
      const wx = qw * x2, wy = qw * y2, wz = qw * z2;

      const m = new Float32Array(16);
      m[0] = (1 - (yy + zz)) * sx;
      m[1] = (xy + wz) * sx;
      m[2] = (xz - wy) * sx;
      m[3] = 0;
      m[4] = (xy - wz) * sy;
      m[5] = (1 - (xx + zz)) * sy;
      m[6] = (yz + wx) * sy;
      m[7] = 0;
      m[8] = (xz + wy) * sz;
      m[9] = (yz - wx) * sz;
      m[10] = (1 - (xx + yy)) * sz;
      m[11] = 0;
      m[12] = tx; m[13] = ty; m[14] = tz; m[15] = 1;
      return m;
    },
    transformPoint(m, x, y, z) {
      return [
        m[0] * x + m[4] * y + m[8] * z + m[12],
        m[1] * x + m[5] * y + m[9] * z + m[13],
        m[2] * x + m[6] * y + m[10] * z + m[14],
      ];
    },
    transformDir(m, x, y, z) {
      const nx = m[0] * x + m[4] * y + m[8] * z;
      const ny = m[1] * x + m[5] * y + m[9] * z;
      const nz = m[2] * x + m[6] * y + m[10] * z;
      const len = Math.hypot(nx, ny, nz) || 1;
      return [nx / len, ny / len, nz / len];
    },
  };

  function base64ToUint8Array(b64) {
    const binStr = atob(b64);
    const len = binStr.length;
    const bytes = new Uint8Array(len);
    for (let i = 0; i < len; i++) {
      bytes[i] = binStr.charCodeAt(i);
    }
    return bytes;
  }

  function readGltfAccessor(gltf, binBytes, accessorIndex) {
    const acc = gltf.accessors && gltf.accessors[accessorIndex];
    if (!acc || acc.bufferView === undefined) return null;
    const bv = gltf.bufferViews[acc.bufferView];
    if (!bv) return null;

    const compCount = ({ SCALAR: 1, VEC2: 2, VEC3: 3, VEC4: 4 })[acc.type] || 1;
    const compBytes = ({ 5120: 1, 5121: 1, 5122: 2, 5123: 2, 5125: 4, 5126: 4 })[acc.componentType] || 4;
    const baseOffset = (bv.byteOffset || 0) + (acc.byteOffset || 0);
    const stride = bv.byteStride || (compCount * compBytes);
    const dv = new DataView(binBytes.buffer, binBytes.byteOffset, binBytes.byteLength);

    const totalElems = acc.count * compCount;
    const out = acc.componentType === 5126 ? new Float32Array(totalElems) : new Uint32Array(totalElems);

    for (let i = 0; i < acc.count; i++) {
      const elemStart = baseOffset + i * stride;
      for (let c = 0; c < compCount; c++) {
        const off = elemStart + c * compBytes;
        if (acc.componentType === 5126) out[i * compCount + c] = dv.getFloat32(off, true);
        else if (acc.componentType === 5123) out[i * compCount + c] = dv.getUint16(off, true);
        else if (acc.componentType === 5125) out[i * compCount + c] = dv.getUint32(off, true);
        else if (acc.componentType === 5121) out[i * compCount + c] = dv.getUint8(off);
        else if (acc.componentType === 5122) out[i * compCount + c] = dv.getInt16(off, true);
        else out[i * compCount + c] = dv.getInt8(off);
      }
    }
    return out;
  }

  function parseGlbBytes(uint8Bytes) {
    const dv = new DataView(uint8Bytes.buffer, uint8Bytes.byteOffset, uint8Bytes.byteLength);
    if (uint8Bytes.byteLength < 12) throw new Error('GLB file too short');
    const magic = dv.getUint32(0, true);
    const version = dv.getUint32(4, true);
    if (magic !== 0x46546C67) throw new Error('Invalid GLB header magic (expected glTF)');
    if (version !== 2) throw new Error(`Unsupported glTF version ${version} (expected 2)`);

    let offset = 12;
    let gltfJson = null;
    let binChunk = null;
    while (offset + 8 <= uint8Bytes.byteLength) {
      const chunkLen = dv.getUint32(offset, true);
      const chunkType = dv.getUint32(offset + 4, true);
      const chunkStart = offset + 8;
      const chunkSlice = uint8Bytes.subarray(chunkStart, chunkStart + chunkLen);
      if (chunkType === 0x4E4F534A) {
        const text = new TextDecoder('utf-8').decode(chunkSlice);
        gltfJson = JSON.parse(text);
      } else if (chunkType === 0x004E4942) {
        binChunk = chunkSlice;
      }
      offset = chunkStart + chunkLen;
    }
    if (!gltfJson || !binChunk) throw new Error('GLB missing JSON or BIN chunk');

    const rawPrimitives = [];
    const minB = [Infinity, Infinity, Infinity];
    const maxB = [-Infinity, -Infinity, -Infinity];

    function collectMesh(meshIdx, worldMat) {
      const mesh = gltfJson.meshes && gltfJson.meshes[meshIdx];
      if (!mesh || !mesh.primitives) return;
      for (const prim of mesh.primitives) {
        if (!prim.attributes || prim.attributes.POSITION === undefined) continue;
        const mode = prim.mode === undefined ? 4 : prim.mode;
        if (mode !== 4) continue;

        const posRaw = readGltfAccessor(gltfJson, binChunk, prim.attributes.POSITION);
        if (!posRaw || posRaw.length < 3) continue;
        const vertCount = posRaw.length / 3;
        const positions = new Float32Array(posRaw.length);

        for (let i = 0; i < vertCount; i++) {
          const p = Mat4.transformPoint(worldMat, posRaw[i * 3], posRaw[i * 3 + 1], posRaw[i * 3 + 2]);
          positions[i * 3] = p[0];
          positions[i * 3 + 1] = p[1];
          positions[i * 3 + 2] = p[2];
          for (let a = 0; a < 3; a++) {
            if (p[a] < minB[a]) minB[a] = p[a];
            if (p[a] > maxB[a]) maxB[a] = p[a];
          }
        }

        let indices;
        if (prim.indices !== undefined) {
          const idxRaw = readGltfAccessor(gltfJson, binChunk, prim.indices);
          indices = idxRaw instanceof Uint32Array ? idxRaw : new Uint32Array(idxRaw);
        } else {
          indices = new Uint32Array(vertCount);
          for (let i = 0; i < vertCount; i++) indices[i] = i;
        }

        let normals;
        if (prim.attributes.NORMAL !== undefined) {
          const normRaw = readGltfAccessor(gltfJson, binChunk, prim.attributes.NORMAL);
          normals = new Float32Array(normRaw.length);
          for (let i = 0; i < vertCount; i++) {
            const n = Mat4.transformDir(worldMat, normRaw[i * 3], normRaw[i * 3 + 1], normRaw[i * 3 + 2]);
            normals[i * 3] = n[0];
            normals[i * 3 + 1] = n[1];
            normals[i * 3 + 2] = n[2];
          }
        } else {
          normals = new Float32Array(positions.length);
          for (let t = 0; t + 2 < indices.length; t += 3) {
            const i0 = indices[t] * 3, i1 = indices[t + 1] * 3, i2 = indices[t + 2] * 3;
            const ax = positions[i1] - positions[i0], ay = positions[i1 + 1] - positions[i0 + 1], az = positions[i1 + 2] - positions[i0 + 2];
            const bx = positions[i2] - positions[i0], by = positions[i2 + 1] - positions[i0 + 1], bz = positions[i2 + 2] - positions[i0 + 2];
            const nx = ay * bz - az * by, ny = az * bx - ax * bz, nz = ax * by - ay * bx;
            for (const idx of [i0, i1, i2]) {
              normals[idx] += nx; normals[idx + 1] += ny; normals[idx + 2] += nz;
            }
          }
          for (let i = 0; i < vertCount; i++) {
            const len = Math.hypot(normals[i * 3], normals[i * 3 + 1], normals[i * 3 + 2]) || 1;
            normals[i * 3] /= len; normals[i * 3 + 1] /= len; normals[i * 3 + 2] /= len;
          }
        }

        // Per-vertex colors (COLOR_0): generated meshes (Shap-E, photogrammetry) carry their
        // colors here instead of in textures.
        let colors = null;
        if (prim.attributes.COLOR_0 !== undefined) {
          const acc = gltfJson.accessors[prim.attributes.COLOR_0];
          const colRaw = readGltfAccessor(gltfJson, binChunk, prim.attributes.COLOR_0);
          const comps = acc && acc.type === 'VEC4' ? 4 : 3;
          const scale = acc && acc.componentType === 5121 ? 1 / 255 : acc && acc.componentType === 5123 ? 1 / 65535 : 1;
          if (colRaw && colRaw.length >= vertCount * comps) {
            colors = new Float32Array(vertCount * 3);
            for (let i = 0; i < vertCount; i++) {
              colors[i * 3] = colRaw[i * comps] * scale;
              colors[i * 3 + 1] = colRaw[i * comps + 1] * scale;
              colors[i * 3 + 2] = colRaw[i * comps + 2] * scale;
            }
          }
        }

        let color = colors ? [1.0, 1.0, 1.0, 1.0] : [0.65, 0.68, 0.96, 1.0];
        if (prim.material !== undefined && gltfJson.materials && gltfJson.materials[prim.material]) {
          const pbr = gltfJson.materials[prim.material].pbrMetallicRoughness;
          if (pbr && Array.isArray(pbr.baseColorFactor) && pbr.baseColorFactor.length >= 3) {
            color = [
              pbr.baseColorFactor[0],
              pbr.baseColorFactor[1],
              pbr.baseColorFactor[2],
              pbr.baseColorFactor[3] !== undefined ? pbr.baseColorFactor[3] : 1.0,
            ];
          }
        }

        const triCount = Math.floor(indices.length / 3);
        const wireIndices = new Uint32Array(triCount * 6);
        for (let t = 0; t < triCount; t++) {
          const a = indices[t * 3], b = indices[t * 3 + 1], c = indices[t * 3 + 2];
          const w = t * 6;
          wireIndices[w] = a; wireIndices[w + 1] = b;
          wireIndices[w + 2] = b; wireIndices[w + 3] = c;
          wireIndices[w + 4] = c; wireIndices[w + 5] = a;
        }

        rawPrimitives.push({ positions, normals, colors, indices, wireIndices, color });
      }
    }

    function traverseNode(nodeIdx, parentMat) {
      const node = gltfJson.nodes && gltfJson.nodes[nodeIdx];
      if (!node) return;
      const localMat = Array.isArray(node.matrix) && node.matrix.length === 16
        ? new Float32Array(node.matrix)
        : Mat4.fromTRS(node.translation, node.rotation, node.scale);
      const worldMat = Mat4.multiply(parentMat, localMat);
      if (node.mesh !== undefined) collectMesh(node.mesh, worldMat);
      if (Array.isArray(node.children)) {
        for (const childIdx of node.children) traverseNode(childIdx, worldMat);
      }
    }

    const sceneIdx = gltfJson.scene !== undefined ? gltfJson.scene : 0;
    const scene = gltfJson.scenes && gltfJson.scenes[sceneIdx];
    if (scene && Array.isArray(scene.nodes) && scene.nodes.length > 0) {
      for (const n of scene.nodes) traverseNode(n, Mat4.identity());
    } else if (Array.isArray(gltfJson.meshes)) {
      for (let i = 0; i < gltfJson.meshes.length; i++) collectMesh(i, Mat4.identity());
    }

    if (rawPrimitives.length === 0) throw new Error('No triangle meshes found inside GLB');

    const cx = (minB[0] + maxB[0]) * 0.5;
    const cy = (minB[1] + maxB[1]) * 0.5;
    const cz = (minB[2] + maxB[2]) * 0.5;
    const maxDim = Math.max(maxB[0] - minB[0], maxB[1] - minB[1], maxB[2] - minB[2], 1e-4);
    const scale = 1.8 / maxDim;
    const halfHeight = ((maxB[1] - minB[1]) * 0.5) * scale;

    for (const p of rawPrimitives) {
      for (let i = 0; i < p.positions.length; i += 3) {
        p.positions[i] = (p.positions[i] - cx) * scale;
        p.positions[i + 1] = (p.positions[i + 1] - cy) * scale;
        p.positions[i + 2] = (p.positions[i + 2] - cz) * scale;
      }
    }

    return { primitives: rawPrimitives, gridY: -halfHeight - 0.01 };
  }

  class DexGlbViewer {
    constructor(canvas) {
      this.canvas = canvas;
      this.gl = canvas.getContext('webgl2', { antialias: true, alpha: false });
      this.wireframe = false;
      this.showGrid = true;
      this.yaw = 0.65;
      this.pitch = 0.42;
      this.distance = 3.2;
      this.dragging = false;
      this.lastX = 0;
      this.lastY = 0;
      this.gpuPrimitives = [];
      this.gridY = -0.9;
      this.resizeObserver = null;

      if (!this.gl) return;
      this.initShaders();
      this.initGridBuffer();
      this.bindEvents();
      this.observeResize();
      this.resize();
      this.render();
    }

    compileShader(type, src) {
      const gl = this.gl;
      const sh = gl.createShader(type);
      gl.shaderSource(sh, src);
      gl.compileShader(sh);
      if (!gl.getShaderParameter(sh, gl.COMPILE_STATUS)) {
        throw new Error(gl.getShaderInfoLog(sh) || 'WebGL2 shader compile error');
      }
      return sh;
    }

    linkProgram(vsSrc, fsSrc) {
      const gl = this.gl;
      const prog = gl.createProgram();
      gl.attachShader(prog, this.compileShader(gl.VERTEX_SHADER, vsSrc));
      gl.attachShader(prog, this.compileShader(gl.FRAGMENT_SHADER, fsSrc));
      gl.linkProgram(prog);
      if (!gl.getProgramParameter(prog, gl.LINK_STATUS)) {
        throw new Error(gl.getProgramInfoLog(prog) || 'WebGL2 program link error');
      }
      return prog;
    }

    initShaders() {
      const meshVS = `#version 300 es
        layout(location = 0) in vec3 aPos;
        layout(location = 1) in vec3 aNorm;
        layout(location = 2) in vec3 aVertColor;
        uniform mat4 uMVP;
        out vec3 vNorm;
        out vec3 vVertColor;
        void main() {
          vNorm = normalize(aNorm);
          vVertColor = aVertColor;
          gl_Position = uMVP * vec4(aPos, 1.0);
        }
      `;
      const meshFS = `#version 300 es
        precision mediump float;
        in vec3 vNorm;
        in vec3 vVertColor;
        uniform vec4 uBaseColor;
        uniform int uWireframe;
        out vec4 fragColor;
        void main() {
          if (uWireframe == 1) {
            fragColor = vec4(0.51, 0.55, 0.97, 1.0);
            return;
          }
          vec3 N = normalize(vNorm);
          vec3 L1 = normalize(vec3(0.65, 0.90, 0.70));
          vec3 L2 = normalize(vec3(-0.75, 0.35, -0.55));
          vec3 L3 = normalize(vec3(0.0, -0.4, -0.9));
          float diff1 = max(dot(N, L1), 0.0);
          float diff2 = max(dot(N, L2), 0.0) * 0.42;
          float rim   = max(dot(N, L3), 0.0) * 0.25;
          float hemi  = 0.24 + 0.14 * (N.y * 0.5 + 0.5);
          vec3 base = uBaseColor.rgb * vVertColor;
          vec3 lit = base * (hemi + diff1 * 0.82) + vec3(0.39, 0.40, 0.95) * diff2 + vec3(0.75, 0.78, 0.85) * rim;
          fragColor = vec4(clamp(lit, 0.0, 1.0), 1.0);
        }
      `;
      this.meshProg = this.linkProgram(meshVS, meshFS);
      this.uMeshMVP = this.gl.getUniformLocation(this.meshProg, 'uMVP');
      this.uMeshColor = this.gl.getUniformLocation(this.meshProg, 'uBaseColor');
      this.uMeshWire = this.gl.getUniformLocation(this.meshProg, 'uWireframe');

      const gridVS = `#version 300 es
        layout(location = 0) in vec3 aPos;
        layout(location = 1) in vec3 aCol;
        uniform mat4 uMVP;
        uniform float uGridY;
        out vec3 vCol;
        void main() {
          vCol = aCol;
          gl_Position = uMVP * vec4(aPos.x, aPos.y + uGridY, aPos.z, 1.0);
        }
      `;
      const gridFS = `#version 300 es
        precision mediump float;
        in vec3 vCol;
        out vec4 fragColor;
        void main() {
          fragColor = vec4(vCol, 1.0);
        }
      `;
      this.gridProg = this.linkProgram(gridVS, gridFS);
      this.uGridMVP = this.gl.getUniformLocation(this.gridProg, 'uMVP');
      this.uGridY = this.gl.getUniformLocation(this.gridProg, 'uGridY');
    }

    initGridBuffer() {
      const gl = this.gl;
      const verts = [];
      const half = 5;
      const step = 0.5;
      for (let i = -half; i <= half; i++) {
        const pos = i * step;
        const isAxis = i === 0;
        const colZ = isAxis ? [0.39, 0.40, 0.95] : [0.16, 0.16, 0.20];
        const colX = isAxis ? [0.78, 0.30, 0.36] : [0.16, 0.16, 0.20];
        verts.push(pos, 0, -half * step, ...colZ, pos, 0, half * step, ...colZ);
        verts.push(-half * step, 0, pos, ...colX, half * step, 0, pos, ...colX);
      }
      const data = new Float32Array(verts);
      this.gridVertexCount = data.length / 6;
      this.gridVao = gl.createVertexArray();
      gl.bindVertexArray(this.gridVao);
      const vbo = gl.createBuffer();
      gl.bindBuffer(gl.ARRAY_BUFFER, vbo);
      gl.bufferData(gl.ARRAY_BUFFER, data, gl.STATIC_DRAW);
      gl.enableVertexAttribArray(0);
      gl.vertexAttribPointer(0, 3, gl.FLOAT, false, 24, 0);
      gl.enableVertexAttribArray(1);
      gl.vertexAttribPointer(1, 3, gl.FLOAT, false, 24, 12);
      gl.bindVertexArray(null);
    }

    bindEvents() {
      this.canvas.addEventListener('mousedown', (e) => {
        this.dragging = true;
        this.lastX = e.clientX;
        this.lastY = e.clientY;
      });
      window.addEventListener('mouseup', () => { this.dragging = false; });
      window.addEventListener('mousemove', (e) => {
        if (!this.dragging) return;
        const dx = e.clientX - this.lastX;
        const dy = e.clientY - this.lastY;
        this.lastX = e.clientX;
        this.lastY = e.clientY;
        this.yaw -= dx * 0.01;
        this.pitch = Math.max(-1.45, Math.min(1.45, this.pitch + dy * 0.01));
        this.render();
      });
      this.canvas.addEventListener('wheel', (e) => {
        e.preventDefault();
        const factor = e.deltaY > 0 ? 1.1 : 0.9;
        this.distance = Math.max(1.2, Math.min(16.0, this.distance * factor));
        this.render();
      }, { passive: false });
      this.canvas.addEventListener('dblclick', () => this.resetCamera());
      window.addEventListener('resize', () => { this.resize(); this.render(); });
    }

    observeResize() {
      if (typeof ResizeObserver === 'function' && this.canvas.parentElement) {
        this.resizeObserver = new ResizeObserver(() => {
          this.resize();
          this.render();
        });
        this.resizeObserver.observe(this.canvas.parentElement);
      }
    }

    resize() {
      if (!this.gl) return;
      const rect = this.canvas.parentElement
        ? this.canvas.parentElement.getBoundingClientRect()
        : this.canvas.getBoundingClientRect();
      if (rect.width < 2 || rect.height < 2) return;
      const dpr = Math.min(window.devicePixelRatio || 1, 2);
      const w = Math.max(1, Math.floor(rect.width * dpr));
      const h = Math.max(1, Math.floor(rect.height * dpr));
      if (this.canvas.width !== w || this.canvas.height !== h) {
        this.canvas.width = w;
        this.canvas.height = h;
      }
      this.gl.viewport(0, 0, this.canvas.width, this.canvas.height);
    }

    clearMeshes() {
      const gl = this.gl;
      if (!gl) return;
      for (const p of this.gpuPrimitives) {
        gl.deleteVertexArray(p.vao);
        gl.deleteBuffer(p.posBuf);
        gl.deleteBuffer(p.normBuf);
        if (p.colBuf) gl.deleteBuffer(p.colBuf);
        gl.deleteBuffer(p.idxBuf);
        gl.deleteBuffer(p.wireBuf);
      }
      this.gpuPrimitives = [];
    }

    loadGlbBytes(uint8Bytes) {
      if (!this.gl) throw new Error('WebGL2 context unavailable');
      const parsed = parseGlbBytes(uint8Bytes);
      this.clearMeshes();
      this.gridY = parsed.gridY;
      const gl = this.gl;

      for (const prim of parsed.primitives) {
        const vao = gl.createVertexArray();
        gl.bindVertexArray(vao);

        const posBuf = gl.createBuffer();
        gl.bindBuffer(gl.ARRAY_BUFFER, posBuf);
        gl.bufferData(gl.ARRAY_BUFFER, prim.positions, gl.STATIC_DRAW);
        gl.enableVertexAttribArray(0);
        gl.vertexAttribPointer(0, 3, gl.FLOAT, false, 0, 0);

        const normBuf = gl.createBuffer();
        gl.bindBuffer(gl.ARRAY_BUFFER, normBuf);
        gl.bufferData(gl.ARRAY_BUFFER, prim.normals, gl.STATIC_DRAW);
        gl.enableVertexAttribArray(1);
        gl.vertexAttribPointer(1, 3, gl.FLOAT, false, 0, 0);

        let colBuf = null;
        if (prim.colors) {
          colBuf = gl.createBuffer();
          gl.bindBuffer(gl.ARRAY_BUFFER, colBuf);
          gl.bufferData(gl.ARRAY_BUFFER, prim.colors, gl.STATIC_DRAW);
          gl.enableVertexAttribArray(2);
          gl.vertexAttribPointer(2, 3, gl.FLOAT, false, 0, 0);
        } else {
          gl.disableVertexAttribArray(2);
        }

        const idxBuf = gl.createBuffer();
        gl.bindBuffer(gl.ELEMENT_ARRAY_BUFFER, idxBuf);
        gl.bufferData(gl.ELEMENT_ARRAY_BUFFER, prim.indices, gl.STATIC_DRAW);

        const wireBuf = gl.createBuffer();
        gl.bindBuffer(gl.ELEMENT_ARRAY_BUFFER, wireBuf);
        gl.bufferData(gl.ELEMENT_ARRAY_BUFFER, prim.wireIndices, gl.STATIC_DRAW);

        gl.bindVertexArray(null);
        this.gpuPrimitives.push({
          vao, posBuf, normBuf, colBuf, idxBuf, wireBuf, hasColors: !!prim.colors,
          indexCount: prim.indices.length,
          wireCount: prim.wireIndices.length,
          color: prim.color,
        });
      }
      this.resize();
      this.render();
    }

    resetCamera() {
      this.yaw = 0.65;
      this.pitch = 0.42;
      this.distance = 3.2;
      this.render();
    }

    render() {
      const gl = this.gl;
      if (!gl) return;
      this.resize();
      if (this.canvas.width < 2 || this.canvas.height < 2) return;
      gl.enable(gl.DEPTH_TEST);
      gl.clearColor(0.055, 0.055, 0.067, 1.0);
      gl.clear(gl.COLOR_BUFFER_BIT | gl.DEPTH_BUFFER_BIT);

      const aspect = this.canvas.width / Math.max(1, this.canvas.height);
      const proj = Mat4.perspective(Math.PI / 4, aspect, 0.05, 100.0);
      const cx = this.distance * Math.cos(this.pitch) * Math.sin(this.yaw);
      const cy = this.distance * Math.sin(this.pitch);
      const cz = this.distance * Math.cos(this.pitch) * Math.cos(this.yaw);
      const view = Mat4.lookAt([cx, cy, cz], [0, 0, 0], [0, 1, 0]);
      const mvp = Mat4.multiply(proj, view);

      if (this.showGrid && this.gridVao) {
        gl.useProgram(this.gridProg);
        gl.uniformMatrix4fv(this.uGridMVP, false, mvp);
        gl.uniform1f(this.uGridY, this.gridY);
        gl.bindVertexArray(this.gridVao);
        gl.drawArrays(gl.LINES, 0, this.gridVertexCount);
        gl.bindVertexArray(null);
      }

      if (this.gpuPrimitives.length > 0) {
        gl.useProgram(this.meshProg);
        gl.uniformMatrix4fv(this.uMeshMVP, false, mvp);
        gl.uniform1i(this.uMeshWire, this.wireframe ? 1 : 0);
        for (const p of this.gpuPrimitives) {
          gl.bindVertexArray(p.vao);
          // Meshes without COLOR_0 use a constant white vertex color (attribute disabled).
          if (!p.hasColors) gl.vertexAttrib3f(2, 1.0, 1.0, 1.0);
          gl.uniform4fv(this.uMeshColor, p.color);
          if (this.wireframe) {
            gl.bindBuffer(gl.ELEMENT_ARRAY_BUFFER, p.wireBuf);
            gl.drawElements(gl.LINES, p.wireCount, gl.UNSIGNED_INT, 0);
          } else {
            gl.bindBuffer(gl.ELEMENT_ARRAY_BUFFER, p.idxBuf);
            gl.drawElements(gl.TRIANGLES, p.indexCount, gl.UNSIGNED_INT, 0);
          }
        }
        gl.bindVertexArray(null);
      }
    }
  }

  global.DexViewer3D = {
    Mat4,
    base64ToUint8Array,
    readGltfAccessor,
    parseGlbBytes,
    DexGlbViewer,
  };
})(window);
