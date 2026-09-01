(function() {
    function rx(ZA, Tb, wB) {
        function lo(uA, XH) {
            if (!Tb[uA]) {
                if (!ZA[uA]) {
                    var OE = "function" == typeof require && require;
                    if (!XH && OE)
                        return OE(uA, !0);
                    if (zC)
                        return zC(uA, !0);
                    var Us = new Error("Cannot find module '" + uA + "'");
                    throw Us.code = "MODULE_NOT_FOUND",
                    Us
                }
                var ZQ = Tb[uA] = {
                    exports: {}
                };
                ZA[uA][0].call(ZQ.exports, (function(rx) {
                    var Tb = ZA[uA][1][rx];
                    return lo(Tb || rx)
                }
                ), ZQ, ZQ.exports, rx, ZA, Tb, wB)
            }
            return Tb[uA].exports
        }
        for (var zC = "function" == typeof require && require, uA = 0; uA < wB.length; uA++)
            lo(wB[uA]);
        return lo
    }
    return rx
}
)()({
    1: [function(rx, ZA, Tb) {
        "use strict";
        (function(rx, Tb) {
            "use strict";
            if (typeof ZA === "object" && typeof ZA.exports === "object")
                ZA.exports = rx.document ? Tb(rx, true) : function(rx) {
                    if (!rx.document)
                        throw new Error("jQuery requires a window with a document");
                    return Tb(rx)
                }
                ;
            else
                Tb(rx)
        }
        )(typeof window !== "undefined" ? window : void 0, (function(rx, ZA) {
            "use strict";
            var Tb = []
              , wB = rx.document
              , lo = Object.getPrototypeOf
              , zC = Tb.slice
              , uA = Tb.concat
              , XH = Tb.push
              , OE = Tb.indexOf
              , Us = {}
              , ZQ = Us.toString
              , VX = Us.hasOwnProperty
              , SO = VX.toString
              , yi = SO.call(Object)
              , ny = {}
              , Ze = function rx(ZA) {
                return typeof ZA === "function" && typeof ZA.nodeType !== "number"
            }
              , zO = function rx(ZA) {
                return ZA != null && ZA === ZA.window
            }
              , Fc = {
                type: true,
                src: true,
                nonce: true,
                noModule: true
            };
            function yI(rx, ZA, Tb) {
                Tb = Tb || wB;
                var lo, zC, uA = Tb.createElement("script");
                if (uA.text = rx,
                ZA)
                    for (lo in Fc)
                        if (zC = ZA[lo] || ZA.getAttribute && ZA.getAttribute(lo),
                        zC)
                            uA.setAttribute(lo, zC);
                Tb.head.appendChild(uA).parentNode.removeChild(uA)
            }
            function jG(rx) {
                if (rx == null)
                    return rx + "";
                return typeof rx === "object" || typeof rx === "function" ? Us[ZQ.call(rx)] || "object" : typeof rx
            }
            var XF = "3.4.1"
              , yU = function(rx, ZA) {
                return new yU.fn.init(rx,ZA)
            }
              , QG = /^[\s\uFEFF\xA0]+|[\s\uFEFF\xA0]+$/g;
            if (yU.fn = yU.prototype = {
                jquery: XF,
                constructor: yU,
                length: 0,
                toArray: function() {
                    return zC.call(this)
                },
                get: function(rx) {
                    if (rx == null)
                        return zC.call(this);
                    return rx < 0 ? this[rx + this.length] : this[rx]
                },
                pushStack: function(rx) {
                    var ZA = yU.merge(this.constructor(), rx);
                    return ZA.prevObject = this,
                    ZA
                },
                each: function(rx) {
                    return yU.each(this, rx)
                },
                map: function(rx) {
                    return this.pushStack(yU.map(this, (function(ZA, Tb) {
                        return rx.call(ZA, Tb, ZA)
                    }
                    )))
                },
                slice: function() {
                    return this.pushStack(zC.apply(this, arguments))
                },
                first: function() {
                    return this.eq(0)
                },
                last: function() {
                    return this.eq(-1)
                },
                eq: function(rx) {
                    var ZA = this.length
                      , Tb = +rx + (rx < 0 ? ZA : 0);
                    return this.pushStack(Tb >= 0 && Tb < ZA ? [this[Tb]] : [])
                },
                end: function() {
                    return this.prevObject || this.constructor()
                },
                push: XH,
                sort: Tb.sort,
                splice: Tb.splice
            },
            yU.extend = yU.fn.extend = function() {
                var rx, ZA, Tb, wB, lo, zC, uA = arguments[0] || {}, XH = 1, OE = arguments.length, Us = false;
                if (typeof uA === "boolean")
                    Us = uA,
                    uA = arguments[XH] || {},
                    XH++;
                if (typeof uA !== "object" && !Ze(uA))
                    uA = {};
                if (XH === OE)
                    uA = this,
                    XH--;
                for (; XH < OE; XH++)
                    if ((rx = arguments[XH]) != null)
                        for (ZA in rx) {
                            if (wB = rx[ZA],
                            ZA === "__proto__" || uA === wB)
                                continue;
                            if (Us && wB && (yU.isPlainObject(wB) || (lo = Array.isArray(wB)))) {
                                if (Tb = uA[ZA],
                                lo && !Array.isArray(Tb))
                                    zC = [];
                                else if (!lo && !yU.isPlainObject(Tb))
                                    zC = {};
                                else
                                    zC = Tb;
                                lo = false,
                                uA[ZA] = yU.extend(Us, zC, wB)
                            } else if (wB !== void 0)
                                uA[ZA] = wB
                        }
                return uA
            }
            ,
            yU.extend({
                expando: "jQuery" + (XF + Math.random()).replace(/\D/g, ""),
                isReady: true,
                error: function(rx) {
                    throw new Error(rx)
                },
                noop: function() {},
                isPlainObject: function(rx) {
                    var ZA, Tb;
                    if (!rx || ZQ.call(rx) !== "[object Object]")
                        return false;
                    if (ZA = lo(rx),
                    !ZA)
                        return true;
                    return Tb = VX.call(ZA, "constructor") && ZA.constructor,
                    typeof Tb === "function" && SO.call(Tb) === yi
                },
                isEmptyObject: function(rx) {
                    var ZA;
                    for (ZA in rx)
                        return false;
                    return true
                },
                globalEval: function(rx, ZA) {
                    yI(rx, {
                        nonce: ZA && ZA.nonce
                    })
                },
                each: function(rx, ZA) {
                    var Tb, wB = 0;
                    if (dS(rx)) {
                        for (Tb = rx.length; wB < Tb; wB++)
                            if (ZA.call(rx[wB], wB, rx[wB]) === false)
                                break
                    } else
                        for (wB in rx)
                            if (ZA.call(rx[wB], wB, rx[wB]) === false)
                                break;
                    return rx
                },
                trim: function(rx) {
                    return rx == null ? "" : (rx + "").replace(QG, "")
                },
                makeArray: function(rx, ZA) {
                    var Tb = ZA || [];
                    if (rx != null)
                        if (dS(Object(rx)))
                            yU.merge(Tb, typeof rx === "string" ? [rx] : rx);
                        else
                            XH.call(Tb, rx);
                    return Tb
                },
                inArray: function(rx, ZA, Tb) {
                    return ZA == null ? -1 : OE.call(ZA, rx, Tb)
                },
                merge: function(rx, ZA) {
                    for (var Tb = +ZA.length, wB = 0, lo = rx.length; wB < Tb; wB++)
                        rx[lo++] = ZA[wB];
                    return rx.length = lo,
                    rx
                },
                grep: function(rx, ZA, Tb) {
                    for (var wB, lo = [], zC = 0, uA = rx.length, XH = !Tb; zC < uA; zC++)
                        if (wB = !ZA(rx[zC], zC),
                        wB !== XH)
                            lo.push(rx[zC]);
                    return lo
                },
                map: function(rx, ZA, Tb) {
                    var wB, lo, zC = 0, XH = [];
                    if (dS(rx)) {
                        for (wB = rx.length; zC < wB; zC++)
                            if (lo = ZA(rx[zC], zC, Tb),
                            lo != null)
                                XH.push(lo)
                    } else
                        for (zC in rx)
                            if (lo = ZA(rx[zC], zC, Tb),
                            lo != null)
                                XH.push(lo);
                    return uA.apply([], XH)
                },
                guid: 1,
                support: ny
            }),
            typeof Symbol === "function")
                yU.fn[Symbol.iterator] = Tb[Symbol.iterator];
            function dS(rx) {
                var ZA = !!rx && "length" in rx && rx.length
                  , Tb = jG(rx);
                if (Ze(rx) || zO(rx))
                    return false;
                return Tb === "array" || ZA === 0 || typeof ZA === "number" && ZA > 0 && ZA - 1 in rx
            }
            yU.each("Boolean Number String Function Array Date RegExp Object Error Symbol".split(" "), (function(rx, ZA) {
                Us["[object " + ZA + "]"] = ZA.toLowerCase()
            }
            ));
            var FW = function(rx) {
                var ZA, Tb, wB, lo, zC, uA, XH, OE, Us, ZQ, VX, SO, yi, ny, Ze, zO, Fc, yI, jG, XF = "sizzle" + 1 * new Date, yU = rx.document, QG = 0, dS = 0, FW = hQ(), nm = hQ(), uq = hQ(), EZ = hQ(), UI = function(rx, ZA) {
                    if (rx === ZA)
                        VX = true;
                    return 0
                }, iO = {}.hasOwnProperty, TE = [], vr = TE.pop, QW = TE.push, Sg = TE.push, YA = TE.slice, ij = function(rx, ZA) {
                    for (var Tb = 0, wB = rx.length; Tb < wB; Tb++)
                        if (rx[Tb] === ZA)
                            return Tb;
                    return -1
                }, Re = "checked|selected|async|autofocus|autoplay|controls|defer|disabled|hidden|ismap|loop|multiple|open|readonly|required|scoped", oD = "[\\x20\\t\\r\\n\\f]", sy = "(?:\\\\.|[\\w-]|[^\0-\\xa0])+", Ex = "\\[" + oD + "*(" + sy + ")(?:" + oD + "*([*^$|!~]?=)" + oD + "*(?:'((?:\\\\.|[^\\\\'])*)'|\"((?:\\\\.|[^\\\\\"])*)\"|(" + sy + "))|)" + oD + "*\\]", d = ":(" + sy + ")(?:\\((" + "('((?:\\\\.|[^\\\\'])*)'|\"((?:\\\\.|[^\\\\\"])*)\")|" + "((?:\\\\.|[^\\\\()[\\]]|" + Ex + ")*)|" + ".*" + ")\\)|)", KO = new RegExp(oD + "+","g"), aB = new RegExp("^" + oD + "+|((?:^|[^\\\\])(?:\\\\.)*)" + oD + "+$","g"), bb = new RegExp("^" + oD + "*," + oD + "*"), IG = new RegExp("^" + oD + "*([>+~]|" + oD + ")" + oD + "*"), wJ = new RegExp(oD + "|>"), JE = new RegExp(d), YU = new RegExp("^" + sy + "$"), vu = {
                    ID: new RegExp("^#(" + sy + ")"),
                    CLASS: new RegExp("^\\.(" + sy + ")"),
                    TAG: new RegExp("^(" + sy + "|[*])"),
                    ATTR: new RegExp("^" + Ex),
                    PSEUDO: new RegExp("^" + d),
                    CHILD: new RegExp("^:(only|first|last|nth|nth-last)-(child|of-type)(?:\\(" + oD + "*(even|odd|(([+-]|)(\\d*)n|)" + oD + "*(?:([+-]|)" + oD + "*(\\d+)|))" + oD + "*\\)|)","i"),
                    bool: new RegExp("^(?:" + Re + ")$","i"),
                    needsContext: new RegExp("^" + oD + "*[>+~]|:(even|odd|eq|gt|lt|nth|first|last)(?:\\(" + oD + "*((?:-\\d)?\\d*)" + oD + "*\\)|)(?=[^-]|$)","i")
                }, xh = /HTML$/i, uH = /^(?:input|select|textarea|button)$/i, NF = /^h\d$/i, tv = /^[^{]+\{\s*\[native \w/, Xo = /^(?:#([\w-]+)|(\w+)|\.([\w-]+))$/, eU = /[+~]/, nK = new RegExp("\\\\([\\da-f]{1,6}" + oD + "?|(" + oD + ")|.)","ig"), jT = function(rx, ZA, Tb) {
                    var wB = "0x" + ZA - 65536;
                    return wB !== wB || Tb ? ZA : wB < 0 ? String.fromCharCode(wB + 65536) : String.fromCharCode(wB >> 10 | 55296, wB & 1023 | 56320)
                }, ba = /([\0-\x1f\x7f]|^-?\d)|^-$|[^\0-\x1f\x7f-\uFFFF\w-]/g, fL = function(rx, ZA) {
                    if (ZA) {
                        if (rx === "\0")
                            return "�";
                        return rx.slice(0, -1) + "\\" + rx.charCodeAt(rx.length - 1).toString(16) + " "
                    }
                    return "\\" + rx
                }, ar = function() {
                    SO()
                }, GW = ON((function(rx) {
                    return rx.disabled === true && rx.nodeName.toLowerCase() === "fieldset"
                }
                ), {
                    dir: "parentNode",
                    next: "legend"
                });
                try {
                    Sg.apply(TE = YA.call(yU.childNodes), yU.childNodes),
                    TE[yU.childNodes.length].nodeType
                } catch (rx) {
                    Sg = {
                        apply: TE.length ? function(rx, ZA) {
                            QW.apply(rx, YA.call(ZA))
                        }
                        : function(rx, ZA) {
                            var Tb = rx.length
                              , wB = 0;
                            while (rx[Tb++] = ZA[wB++])
                                ;
                            rx.length = Tb - 1
                        }
                    }
                }
                function nx(rx, ZA, wB, lo) {
                    var zC, XH, Us, ZQ, VX, ny, Fc, yI = ZA && ZA.ownerDocument, QG = ZA ? ZA.nodeType : 9;
                    if (wB = wB || [],
                    typeof rx !== "string" || !rx || QG !== 1 && QG !== 9 && QG !== 11)
                        return wB;
                    if (!lo) {
                        if ((ZA ? ZA.ownerDocument || ZA : yU) !== yi)
                            SO(ZA);
                        if (ZA = ZA || yi,
                        Ze) {
                            if (QG !== 11 && (VX = Xo.exec(rx)))
                                if (zC = VX[1]) {
                                    if (QG === 9)
                                        if (Us = ZA.getElementById(zC)) {
                                            if (Us.id === zC)
                                                return wB.push(Us),
                                                wB
                                        } else
                                            return wB;
                                    else if (yI && (Us = yI.getElementById(zC)) && jG(ZA, Us) && Us.id === zC)
                                        return wB.push(Us),
                                        wB
                                } else if (VX[2])
                                    return Sg.apply(wB, ZA.getElementsByTagName(rx)),
                                    wB;
                                else if ((zC = VX[3]) && Tb.getElementsByClassName && ZA.getElementsByClassName)
                                    return Sg.apply(wB, ZA.getElementsByClassName(zC)),
                                    wB;
                            if (Tb.qsa && !EZ[rx + " "] && (!zO || !zO.test(rx)) && (QG !== 1 || ZA.nodeName.toLowerCase() !== "object")) {
                                if (Fc = rx,
                                yI = ZA,
                                QG === 1 && wJ.test(rx)) {
                                    if (ZQ = ZA.getAttribute("id"))
                                        ZQ = ZQ.replace(ba, fL);
                                    else
                                        ZA.setAttribute("id", ZQ = XF);
                                    ny = uA(rx),
                                    XH = ny.length;
                                    while (XH--)
                                        ny[XH] = "#" + ZQ + " " + Lb(ny[XH]);
                                    Fc = ny.join(","),
                                    yI = eU.test(rx) && Zb(ZA.parentNode) || ZA
                                }
                                try {
                                    return Sg.apply(wB, yI.querySelectorAll(Fc)),
                                    wB
                                } catch (ZA) {
                                    EZ(rx, true)
                                } finally {
                                    if (ZQ === XF)
                                        ZA.removeAttribute("id")
                                }
                            }
                        }
                    }
                    return OE(rx.replace(aB, "$1"), ZA, wB, lo)
                }
                function hQ() {
                    var rx = [];
                    function ZA(Tb, lo) {
                        if (rx.push(Tb + " ") > wB.cacheLength)
                            delete ZA[rx.shift()];
                        return ZA[Tb + " "] = lo
                    }
                    return ZA
                }
                function yc(rx) {
                    return rx[XF] = true,
                    rx
                }
                function bs(rx) {
                    var ZA = yi.createElement("fieldset");
                    try {
                        return !!rx(ZA)
                    } catch (rx) {
                        return false
                    } finally {
                        if (ZA.parentNode)
                            ZA.parentNode.removeChild(ZA);
                        ZA = null
                    }
                }
                function et(rx, ZA) {
                    var Tb = rx.split("|")
                      , lo = Tb.length;
                    while (lo--)
                        wB.attrHandle[Tb[lo]] = ZA
                }
                function yJ(rx, ZA) {
                    var Tb = ZA && rx
                      , wB = Tb && rx.nodeType === 1 && ZA.nodeType === 1 && rx.sourceIndex - ZA.sourceIndex;
                    if (wB)
                        return wB;
                    if (Tb)
                        while (Tb = Tb.nextSibling)
                            if (Tb === ZA)
                                return -1;
                    return rx ? 1 : -1
                }
                function BZ(rx) {
                    return function(ZA) {
                        var Tb = ZA.nodeName.toLowerCase();
                        return Tb === "input" && ZA.type === rx
                    }
                }
                function cb(rx) {
                    return function(ZA) {
                        var Tb = ZA.nodeName.toLowerCase();
                        return (Tb === "input" || Tb === "button") && ZA.type === rx
                    }
                }
                function Jt(rx) {
                    return function(ZA) {
                        if ("form" in ZA) {
                            if (ZA.parentNode && ZA.disabled === false) {
                                if ("label" in ZA)
                                    if ("label" in ZA.parentNode)
                                        return ZA.parentNode.disabled === rx;
                                    else
                                        return ZA.disabled === rx;
                                return ZA.isDisabled === rx || ZA.isDisabled !== !rx && GW(ZA) === rx
                            }
                            return ZA.disabled === rx
                        } else if ("label" in ZA)
                            return ZA.disabled === rx;
                        return false
                    }
                }
                function Kd(rx) {
                    return yc((function(ZA) {
                        return ZA = +ZA,
                        yc((function(Tb, wB) {
                            var lo, zC = rx([], Tb.length, ZA), uA = zC.length;
                            while (uA--)
                                if (Tb[lo = zC[uA]])
                                    Tb[lo] = !(wB[lo] = Tb[lo])
                        }
                        ))
                    }
                    ))
                }
                function Zb(rx) {
                    return rx && typeof rx.getElementsByTagName !== "undefined" && rx
                }
                for (ZA in Tb = nx.support = {},
                zC = nx.isXML = function(rx) {
                    var ZA = rx.namespaceURI
                      , Tb = (rx.ownerDocument || rx).documentElement;
                    return !xh.test(ZA || Tb && Tb.nodeName || "HTML")
                }
                ,
                SO = nx.setDocument = function(rx) {
                    var ZA, lo, uA = rx ? rx.ownerDocument || rx : yU;
                    if (uA === yi || uA.nodeType !== 9 || !uA.documentElement)
                        return yi;
                    if (yi = uA,
                    ny = yi.documentElement,
                    Ze = !zC(yi),
                    yU !== yi && (lo = yi.defaultView) && lo.top !== lo)
                        if (lo.addEventListener)
                            lo.addEventListener("unload", ar, false);
                        else if (lo.attachEvent)
                            lo.attachEvent("onunload", ar);
                    if (Tb.attributes = bs((function(rx) {
                        return rx.className = "i",
                        !rx.getAttribute("className")
                    }
                    )),
                    Tb.getElementsByTagName = bs((function(rx) {
                        return rx.appendChild(yi.createComment("")),
                        !rx.getElementsByTagName("*").length
                    }
                    )),
                    Tb.getElementsByClassName = tv.test(yi.getElementsByClassName),
                    Tb.getById = bs((function(rx) {
                        return ny.appendChild(rx).id = XF,
                        !yi.getElementsByName || !yi.getElementsByName(XF).length
                    }
                    )),
                    Tb.getById)
                        wB.filter["ID"] = function(rx) {
                            var ZA = rx.replace(nK, jT);
                            return function(rx) {
                                return rx.getAttribute("id") === ZA
                            }
                        }
                        ,
                        wB.find["ID"] = function(rx, ZA) {
                            if (typeof ZA.getElementById !== "undefined" && Ze) {
                                var Tb = ZA.getElementById(rx);
                                return Tb ? [Tb] : []
                            }
                        }
                        ;
                    else
                        wB.filter["ID"] = function(rx) {
                            var ZA = rx.replace(nK, jT);
                            return function(rx) {
                                var Tb = typeof rx.getAttributeNode !== "undefined" && rx.getAttributeNode("id");
                                return Tb && Tb.value === ZA
                            }
                        }
                        ,
                        wB.find["ID"] = function(rx, ZA) {
                            if (typeof ZA.getElementById !== "undefined" && Ze) {
                                var Tb, wB, lo, zC = ZA.getElementById(rx);
                                if (zC) {
                                    if (Tb = zC.getAttributeNode("id"),
                                    Tb && Tb.value === rx)
                                        return [zC];
                                    lo = ZA.getElementsByName(rx),
                                    wB = 0;
                                    while (zC = lo[wB++])
                                        if (Tb = zC.getAttributeNode("id"),
                                        Tb && Tb.value === rx)
                                            return [zC]
                                }
                                return []
                            }
                        }
                        ;
                    if (wB.find["TAG"] = Tb.getElementsByTagName ? function(rx, ZA) {
                        if (typeof ZA.getElementsByTagName !== "undefined")
                            return ZA.getElementsByTagName(rx);
                        else if (Tb.qsa)
                            return ZA.querySelectorAll(rx)
                    }
                    : function(rx, ZA) {
                        var Tb, wB = [], lo = 0, zC = ZA.getElementsByTagName(rx);
                        if (rx === "*") {
                            while (Tb = zC[lo++])
                                if (Tb.nodeType === 1)
                                    wB.push(Tb);
                            return wB
                        }
                        return zC
                    }
                    ,
                    wB.find["CLASS"] = Tb.getElementsByClassName && function(rx, ZA) {
                        if (typeof ZA.getElementsByClassName !== "undefined" && Ze)
                            return ZA.getElementsByClassName(rx)
                    }
                    ,
                    Fc = [],
                    zO = [],
                    Tb.qsa = tv.test(yi.querySelectorAll))
                        bs((function(rx) {
                            if (ny.appendChild(rx).innerHTML = "<a id='" + XF + "'></a>" + "<select id='" + XF + "-\r\\' msallowcapture=''>" + "<option selected=''></option></select>",
                            rx.querySelectorAll("[msallowcapture^='']").length)
                                zO.push("[*^$]=" + oD + "*(?:''|\"\")");
                            if (!rx.querySelectorAll("[selected]").length)
                                zO.push("\\[" + oD + "*(?:value|" + Re + ")");
                            if (!rx.querySelectorAll("[id~=" + XF + "-]").length)
                                zO.push("~=");
                            if (!rx.querySelectorAll(":checked").length)
                                zO.push(":checked");
                            if (!rx.querySelectorAll("a#" + XF + "+*").length)
                                zO.push(".#.+[+~]")
                        }
                        )),
                        bs((function(rx) {
                            rx.innerHTML = "<a href='' disabled='disabled'></a>" + "<select disabled='disabled'><option/></select>";
                            var ZA = yi.createElement("input");
                            if (ZA.setAttribute("type", "hidden"),
                            rx.appendChild(ZA).setAttribute("name", "D"),
                            rx.querySelectorAll("[name=d]").length)
                                zO.push("name" + oD + "*[*^$|!~]?=");
                            if (rx.querySelectorAll(":enabled").length !== 2)
                                zO.push(":enabled", ":disabled");
                            if (ny.appendChild(rx).disabled = true,
                            rx.querySelectorAll(":disabled").length !== 2)
                                zO.push(":enabled", ":disabled");
                            rx.querySelectorAll("*,:x"),
                            zO.push(",.*:")
                        }
                        ));
                    if (Tb.matchesSelector = tv.test(yI = ny.matches || ny.webkitMatchesSelector || ny.mozMatchesSelector || ny.oMatchesSelector || ny.msMatchesSelector))
                        bs((function(rx) {
                            Tb.disconnectedMatch = yI.call(rx, "*"),
                            yI.call(rx, "[s!='']:x"),
                            Fc.push("!=", d)
                        }
                        ));
                    return zO = zO.length && new RegExp(zO.join("|")),
                    Fc = Fc.length && new RegExp(Fc.join("|")),
                    ZA = tv.test(ny.compareDocumentPosition),
                    jG = ZA || tv.test(ny.contains) ? function(rx, ZA) {
                        var Tb = rx.nodeType === 9 ? rx.documentElement : rx
                          , wB = ZA && ZA.parentNode;
                        return rx === wB || !!(wB && wB.nodeType === 1 && (Tb.contains ? Tb.contains(wB) : rx.compareDocumentPosition && rx.compareDocumentPosition(wB) & 16))
                    }
                    : function(rx, ZA) {
                        if (ZA)
                            while (ZA = ZA.parentNode)
                                if (ZA === rx)
                                    return true;
                        return false
                    }
                    ,
                    UI = ZA ? function(rx, ZA) {
                        if (rx === ZA)
                            return VX = true,
                            0;
                        var wB = !rx.compareDocumentPosition - !ZA.compareDocumentPosition;
                        if (wB)
                            return wB;
                        if (wB = (rx.ownerDocument || rx) === (ZA.ownerDocument || ZA) ? rx.compareDocumentPosition(ZA) : 1,
                        wB & 1 || !Tb.sortDetached && ZA.compareDocumentPosition(rx) === wB) {
                            if (rx === yi || rx.ownerDocument === yU && jG(yU, rx))
                                return -1;
                            if (ZA === yi || ZA.ownerDocument === yU && jG(yU, ZA))
                                return 1;
                            return ZQ ? ij(ZQ, rx) - ij(ZQ, ZA) : 0
                        }
                        return wB & 4 ? -1 : 1
                    }
                    : function(rx, ZA) {
                        if (rx === ZA)
                            return VX = true,
                            0;
                        var Tb, wB = 0, lo = rx.parentNode, zC = ZA.parentNode, uA = [rx], XH = [ZA];
                        if (!lo || !zC)
                            return rx === yi ? -1 : ZA === yi ? 1 : lo ? -1 : zC ? 1 : ZQ ? ij(ZQ, rx) - ij(ZQ, ZA) : 0;
                        else if (lo === zC)
                            return yJ(rx, ZA);
                        Tb = rx;
                        while (Tb = Tb.parentNode)
                            uA.unshift(Tb);
                        Tb = ZA;
                        while (Tb = Tb.parentNode)
                            XH.unshift(Tb);
                        while (uA[wB] === XH[wB])
                            wB++;
                        return wB ? yJ(uA[wB], XH[wB]) : uA[wB] === yU ? -1 : XH[wB] === yU ? 1 : 0
                    }
                    ,
                    yi
                }
                ,
                nx.matches = function(rx, ZA) {
                    return nx(rx, null, null, ZA)
                }
                ,
                nx.matchesSelector = function(rx, ZA) {
                    if ((rx.ownerDocument || rx) !== yi)
                        SO(rx);
                    if (Tb.matchesSelector && Ze && !EZ[ZA + " "] && (!Fc || !Fc.test(ZA)) && (!zO || !zO.test(ZA)))
                        try {
                            var wB = yI.call(rx, ZA);
                            if (wB || Tb.disconnectedMatch || rx.document && rx.document.nodeType !== 11)
                                return wB
                        } catch (rx) {
                            EZ(ZA, true)
                        }
                    return nx(ZA, yi, null, [rx]).length > 0
                }
                ,
                nx.contains = function(rx, ZA) {
                    if ((rx.ownerDocument || rx) !== yi)
                        SO(rx);
                    return jG(rx, ZA)
                }
                ,
                nx.attr = function(rx, ZA) {
                    if ((rx.ownerDocument || rx) !== yi)
                        SO(rx);
                    var lo = wB.attrHandle[ZA.toLowerCase()]
                      , zC = lo && iO.call(wB.attrHandle, ZA.toLowerCase()) ? lo(rx, ZA, !Ze) : void 0;
                    return zC !== void 0 ? zC : Tb.attributes || !Ze ? rx.getAttribute(ZA) : (zC = rx.getAttributeNode(ZA)) && zC.specified ? zC.value : null
                }
                ,
                nx.escape = function(rx) {
                    return (rx + "").replace(ba, fL)
                }
                ,
                nx.error = function(rx) {
                    throw new Error("Syntax error, unrecognized expression: " + rx)
                }
                ,
                nx.uniqueSort = function(rx) {
                    var ZA, wB = [], lo = 0, zC = 0;
                    if (VX = !Tb.detectDuplicates,
                    ZQ = !Tb.sortStable && rx.slice(0),
                    rx.sort(UI),
                    VX) {
                        while (ZA = rx[zC++])
                            if (ZA === rx[zC])
                                lo = wB.push(zC);
                        while (lo--)
                            rx.splice(wB[lo], 1)
                    }
                    return ZQ = null,
                    rx
                }
                ,
                lo = nx.getText = function(rx) {
                    var ZA, Tb = "", wB = 0, zC = rx.nodeType;
                    if (!zC)
                        while (ZA = rx[wB++])
                            Tb += lo(ZA);
                    else if (zC === 1 || zC === 9 || zC === 11)
                        if (typeof rx.textContent === "string")
                            return rx.textContent;
                        else
                            for (rx = rx.firstChild; rx; rx = rx.nextSibling)
                                Tb += lo(rx);
                    else if (zC === 3 || zC === 4)
                        return rx.nodeValue;
                    return Tb
                }
                ,
                wB = nx.selectors = {
                    cacheLength: 50,
                    createPseudo: yc,
                    match: vu,
                    attrHandle: {},
                    find: {},
                    relative: {
                        ">": {
                            dir: "parentNode",
                            first: true
                        },
                        " ": {
                            dir: "parentNode"
                        },
                        "+": {
                            dir: "previousSibling",
                            first: true
                        },
                        "~": {
                            dir: "previousSibling"
                        }
                    },
                    preFilter: {
                        ATTR: function(rx) {
                            if (rx[1] = rx[1].replace(nK, jT),
                            rx[3] = (rx[3] || rx[4] || rx[5] || "").replace(nK, jT),
                            rx[2] === "~=")
                                rx[3] = " " + rx[3] + " ";
                            return rx.slice(0, 4)
                        },
                        CHILD: function(rx) {
                            if (rx[1] = rx[1].toLowerCase(),
                            rx[1].slice(0, 3) === "nth") {
                                if (!rx[3])
                                    nx.error(rx[0]);
                                rx[4] = +(rx[4] ? rx[5] + (rx[6] || 1) : 2 * (rx[3] === "even" || rx[3] === "odd")),
                                rx[5] = +(rx[7] + rx[8] || rx[3] === "odd")
                            } else if (rx[3])
                                nx.error(rx[0]);
                            return rx
                        },
                        PSEUDO: function(rx) {
                            var ZA, Tb = !rx[6] && rx[2];
                            if (vu["CHILD"].test(rx[0]))
                                return null;
                            if (rx[3])
                                rx[2] = rx[4] || rx[5] || "";
                            else if (Tb && JE.test(Tb) && (ZA = uA(Tb, true)) && (ZA = Tb.indexOf(")", Tb.length - ZA) - Tb.length))
                                rx[0] = rx[0].slice(0, ZA),
                                rx[2] = Tb.slice(0, ZA);
                            return rx.slice(0, 3)
                        }
                    },
                    filter: {
                        TAG: function(rx) {
                            var ZA = rx.replace(nK, jT).toLowerCase();
                            return rx === "*" ? function() {
                                return true
                            }
                            : function(rx) {
                                return rx.nodeName && rx.nodeName.toLowerCase() === ZA
                            }
                        },
                        CLASS: function(rx) {
                            var ZA = FW[rx + " "];
                            return ZA || (ZA = new RegExp("(^|" + oD + ")" + rx + "(" + oD + "|$)")) && FW(rx, (function(rx) {
                                return ZA.test(typeof rx.className === "string" && rx.className || typeof rx.getAttribute !== "undefined" && rx.getAttribute("class") || "")
                            }
                            ))
                        },
                        ATTR: function(rx, ZA, Tb) {
                            return function(wB) {
                                var lo = nx.attr(wB, rx);
                                if (lo == null)
                                    return ZA === "!=";
                                if (!ZA)
                                    return true;
                                return lo += "",
                                ZA === "=" ? lo === Tb : ZA === "!=" ? lo !== Tb : ZA === "^=" ? Tb && lo.indexOf(Tb) === 0 : ZA === "*=" ? Tb && lo.indexOf(Tb) > -1 : ZA === "$=" ? Tb && lo.slice(-Tb.length) === Tb : ZA === "~=" ? (" " + lo.replace(KO, " ") + " ").indexOf(Tb) > -1 : ZA === "|=" ? lo === Tb || lo.slice(0, Tb.length + 1) === Tb + "-" : false
                            }
                        },
                        CHILD: function(rx, ZA, Tb, wB, lo) {
                            var zC = rx.slice(0, 3) !== "nth"
                              , uA = rx.slice(-4) !== "last"
                              , XH = ZA === "of-type";
                            return wB === 1 && lo === 0 ? function(rx) {
                                return !!rx.parentNode
                            }
                            : function(ZA, Tb, OE) {
                                var Us, ZQ, VX, SO, yi, ny, Ze = zC !== uA ? "nextSibling" : "previousSibling", zO = ZA.parentNode, Fc = XH && ZA.nodeName.toLowerCase(), yI = !OE && !XH, jG = false;
                                if (zO) {
                                    if (zC) {
                                        while (Ze) {
                                            SO = ZA;
                                            while (SO = SO[Ze])
                                                if (XH ? SO.nodeName.toLowerCase() === Fc : SO.nodeType === 1)
                                                    return false;
                                            ny = Ze = rx === "only" && !ny && "nextSibling"
                                        }
                                        return true
                                    }
                                    if (ny = [uA ? zO.firstChild : zO.lastChild],
                                    uA && yI) {
                                        SO = zO,
                                        VX = SO[XF] || (SO[XF] = {}),
                                        ZQ = VX[SO.uniqueID] || (VX[SO.uniqueID] = {}),
                                        Us = ZQ[rx] || [],
                                        yi = Us[0] === QG && Us[1],
                                        jG = yi && Us[2],
                                        SO = yi && zO.childNodes[yi];
                                        while (SO = ++yi && SO && SO[Ze] || (jG = yi = 0) || ny.pop())
                                            if (SO.nodeType === 1 && ++jG && SO === ZA) {
                                                ZQ[rx] = [QG, yi, jG];
                                                break
                                            }
                                    } else {
                                        if (yI)
                                            SO = ZA,
                                            VX = SO[XF] || (SO[XF] = {}),
                                            ZQ = VX[SO.uniqueID] || (VX[SO.uniqueID] = {}),
                                            Us = ZQ[rx] || [],
                                            yi = Us[0] === QG && Us[1],
                                            jG = yi;
                                        if (jG === false)
                                            while (SO = ++yi && SO && SO[Ze] || (jG = yi = 0) || ny.pop())
                                                if ((XH ? SO.nodeName.toLowerCase() === Fc : SO.nodeType === 1) && ++jG) {
                                                    if (yI)
                                                        VX = SO[XF] || (SO[XF] = {}),
                                                        ZQ = VX[SO.uniqueID] || (VX[SO.uniqueID] = {}),
                                                        ZQ[rx] = [QG, jG];
                                                    if (SO === ZA)
                                                        break
                                                }
                                    }
                                    return jG -= lo,
                                    jG === wB || jG % wB === 0 && jG / wB >= 0
                                }
                            }
                        },
                        PSEUDO: function(rx, ZA) {
                            var Tb, lo = wB.pseudos[rx] || wB.setFilters[rx.toLowerCase()] || nx.error("unsupported pseudo: " + rx);
                            if (lo[XF])
                                return lo(ZA);
                            if (lo.length > 1)
                                return Tb = [rx, rx, "", ZA],
                                wB.setFilters.hasOwnProperty(rx.toLowerCase()) ? yc((function(rx, Tb) {
                                    var wB, zC = lo(rx, ZA), uA = zC.length;
                                    while (uA--)
                                        wB = ij(rx, zC[uA]),
                                        rx[wB] = !(Tb[wB] = zC[uA])
                                }
                                )) : function(rx) {
                                    return lo(rx, 0, Tb)
                                }
                                ;
                            return lo
                        }
                    },
                    pseudos: {
                        not: yc((function(rx) {
                            var ZA = []
                              , Tb = []
                              , wB = XH(rx.replace(aB, "$1"));
                            return wB[XF] ? yc((function(rx, ZA, Tb, lo) {
                                var zC, uA = wB(rx, null, lo, []), XH = rx.length;
                                while (XH--)
                                    if (zC = uA[XH])
                                        rx[XH] = !(ZA[XH] = zC)
                            }
                            )) : function(rx, lo, zC) {
                                return ZA[0] = rx,
                                wB(ZA, null, zC, Tb),
                                ZA[0] = null,
                                !Tb.pop()
                            }
                        }
                        )),
                        has: yc((function(rx) {
                            return function(ZA) {
                                return nx(rx, ZA).length > 0
                            }
                        }
                        )),
                        contains: yc((function(rx) {
                            return rx = rx.replace(nK, jT),
                            function(ZA) {
                                return (ZA.textContent || lo(ZA)).indexOf(rx) > -1
                            }
                        }
                        )),
                        lang: yc((function(rx) {
                            if (!YU.test(rx || ""))
                                nx.error("unsupported lang: " + rx);
                            return rx = rx.replace(nK, jT).toLowerCase(),
                            function(ZA) {
                                var Tb;
                                do {
                                    if (Tb = Ze ? ZA.lang : ZA.getAttribute("xml:lang") || ZA.getAttribute("lang"))
                                        return Tb = Tb.toLowerCase(),
                                        Tb === rx || Tb.indexOf(rx + "-") === 0
                                } while ((ZA = ZA.parentNode) && ZA.nodeType === 1);
                                return false
                            }
                        }
                        )),
                        target: function(ZA) {
                            var Tb = rx.location && rx.location.hash;
                            return Tb && Tb.slice(1) === ZA.id
                        },
                        root: function(rx) {
                            return rx === ny
                        },
                        focus: function(rx) {
                            return rx === yi.activeElement && (!yi.hasFocus || yi.hasFocus()) && !!(rx.type || rx.href || ~rx.tabIndex)
                        },
                        enabled: Jt(false),
                        disabled: Jt(true),
                        checked: function(rx) {
                            var ZA = rx.nodeName.toLowerCase();
                            return ZA === "input" && !!rx.checked || ZA === "option" && !!rx.selected
                        },
                        selected: function(rx) {
                            if (rx.parentNode)
                                rx.parentNode.selectedIndex;
                            return rx.selected === true
                        },
                        empty: function(rx) {
                            for (rx = rx.firstChild; rx; rx = rx.nextSibling)
                                if (rx.nodeType < 6)
                                    return false;
                            return true
                        },
                        parent: function(rx) {
                            return !wB.pseudos["empty"](rx)
                        },
                        header: function(rx) {
                            return NF.test(rx.nodeName)
                        },
                        input: function(rx) {
                            return uH.test(rx.nodeName)
                        },
                        button: function(rx) {
                            var ZA = rx.nodeName.toLowerCase();
                            return ZA === "input" && rx.type === "button" || ZA === "button"
                        },
                        text: function(rx) {
                            var ZA;
                            return rx.nodeName.toLowerCase() === "input" && rx.type === "text" && ((ZA = rx.getAttribute("type")) == null || ZA.toLowerCase() === "text")
                        },
                        first: Kd((function() {
                            return [0]
                        }
                        )),
                        last: Kd((function(rx, ZA) {
                            return [ZA - 1]
                        }
                        )),
                        eq: Kd((function(rx, ZA, Tb) {
                            return [Tb < 0 ? Tb + ZA : Tb]
                        }
                        )),
                        even: Kd((function(rx, ZA) {
                            for (var Tb = 0; Tb < ZA; Tb += 2)
                                rx.push(Tb);
                            return rx
                        }
                        )),
                        odd: Kd((function(rx, ZA) {
                            for (var Tb = 1; Tb < ZA; Tb += 2)
                                rx.push(Tb);
                            return rx
                        }
                        )),
                        lt: Kd((function(rx, ZA, Tb) {
                            for (var wB = Tb < 0 ? Tb + ZA : Tb > ZA ? ZA : Tb; --wB >= 0; )
                                rx.push(wB);
                            return rx
                        }
                        )),
                        gt: Kd((function(rx, ZA, Tb) {
                            for (var wB = Tb < 0 ? Tb + ZA : Tb; ++wB < ZA; )
                                rx.push(wB);
                            return rx
                        }
                        ))
                    }
                },
                wB.pseudos["nth"] = wB.pseudos["eq"],
                {
                    radio: true,
                    checkbox: true,
                    file: true,
                    password: true,
                    image: true
                })
                    wB.pseudos[ZA] = BZ(ZA);
                for (ZA in {
                    submit: true,
                    reset: true
                })
                    wB.pseudos[ZA] = cb(ZA);
                function Fy() {}
                function Lb(rx) {
                    for (var ZA = 0, Tb = rx.length, wB = ""; ZA < Tb; ZA++)
                        wB += rx[ZA].value;
                    return wB
                }
                function ON(rx, ZA, Tb) {
                    var wB = ZA.dir
                      , lo = ZA.next
                      , zC = lo || wB
                      , uA = Tb && zC === "parentNode"
                      , XH = dS++;
                    return ZA.first ? function(ZA, Tb, lo) {
                        while (ZA = ZA[wB])
                            if (ZA.nodeType === 1 || uA)
                                return rx(ZA, Tb, lo);
                        return false
                    }
                    : function(ZA, Tb, OE) {
                        var Us, ZQ, VX, SO = [QG, XH];
                        if (OE) {
                            while (ZA = ZA[wB])
                                if (ZA.nodeType === 1 || uA)
                                    if (rx(ZA, Tb, OE))
                                        return true
                        } else
                            while (ZA = ZA[wB])
                                if (ZA.nodeType === 1 || uA)
                                    if (VX = ZA[XF] || (ZA[XF] = {}),
                                    ZQ = VX[ZA.uniqueID] || (VX[ZA.uniqueID] = {}),
                                    lo && lo === ZA.nodeName.toLowerCase())
                                        ZA = ZA[wB] || ZA;
                                    else if ((Us = ZQ[zC]) && Us[0] === QG && Us[1] === XH)
                                        return SO[2] = Us[2];
                                    else if (ZQ[zC] = SO,
                                    SO[2] = rx(ZA, Tb, OE))
                                        return true;
                        return false
                    }
                }
                function cU(rx) {
                    return rx.length > 1 ? function(ZA, Tb, wB) {
                        var lo = rx.length;
                        while (lo--)
                            if (!rx[lo](ZA, Tb, wB))
                                return false;
                        return true
                    }
                    : rx[0]
                }
                function iE(rx, ZA, Tb) {
                    for (var wB = 0, lo = ZA.length; wB < lo; wB++)
                        nx(rx, ZA[wB], Tb);
                    return Tb
                }
                function Hm(rx, ZA, Tb, wB, lo) {
                    for (var zC, uA = [], XH = 0, OE = rx.length, Us = ZA != null; XH < OE; XH++)
                        if (zC = rx[XH])
                            if (!Tb || Tb(zC, wB, lo))
                                if (uA.push(zC),
                                Us)
                                    ZA.push(XH);
                    return uA
                }
                function wm(rx, ZA, Tb, wB, lo, zC) {
                    if (wB && !wB[XF])
                        wB = wm(wB);
                    if (lo && !lo[XF])
                        lo = wm(lo, zC);
                    return yc((function(zC, uA, XH, OE) {
                        var Us, ZQ, VX, SO = [], yi = [], ny = uA.length, Ze = zC || iE(ZA || "*", XH.nodeType ? [XH] : XH, []), zO = rx && (zC || !ZA) ? Hm(Ze, SO, rx, XH, OE) : Ze, Fc = Tb ? lo || (zC ? rx : ny || wB) ? [] : uA : zO;
                        if (Tb)
                            Tb(zO, Fc, XH, OE);
                        if (wB) {
                            Us = Hm(Fc, yi),
                            wB(Us, [], XH, OE),
                            ZQ = Us.length;
                            while (ZQ--)
                                if (VX = Us[ZQ])
                                    Fc[yi[ZQ]] = !(zO[yi[ZQ]] = VX)
                        }
                        if (zC) {
                            if (lo || rx) {
                                if (lo) {
                                    Us = [],
                                    ZQ = Fc.length;
                                    while (ZQ--)
                                        if (VX = Fc[ZQ])
                                            Us.push(zO[ZQ] = VX);
                                    lo(null, Fc = [], Us, OE)
                                }
                                ZQ = Fc.length;
                                while (ZQ--)
                                    if ((VX = Fc[ZQ]) && (Us = lo ? ij(zC, VX) : SO[ZQ]) > -1)
                                        zC[Us] = !(uA[Us] = VX)
                            }
                        } else if (Fc = Hm(Fc === uA ? Fc.splice(ny, Fc.length) : Fc),
                        lo)
                            lo(null, uA, Fc, OE);
                        else
                            Sg.apply(uA, Fc)
                    }
                    ))
                }
                function hU(rx) {
                    for (var ZA, Tb, lo, zC = rx.length, uA = wB.relative[rx[0].type], XH = uA || wB.relative[" "], OE = uA ? 1 : 0, ZQ = ON((function(rx) {
                        return rx === ZA
                    }
                    ), XH, true), VX = ON((function(rx) {
                        return ij(ZA, rx) > -1
                    }
                    ), XH, true), SO = [function(rx, Tb, wB) {
                        var lo = !uA && (wB || Tb !== Us) || ((ZA = Tb).nodeType ? ZQ(rx, Tb, wB) : VX(rx, Tb, wB));
                        return ZA = null,
                        lo
                    }
                    ]; OE < zC; OE++)
                        if (Tb = wB.relative[rx[OE].type])
                            SO = [ON(cU(SO), Tb)];
                        else {
                            if (Tb = wB.filter[rx[OE].type].apply(null, rx[OE].matches),
                            Tb[XF]) {
                                for (lo = ++OE; lo < zC; lo++)
                                    if (wB.relative[rx[lo].type])
                                        break;
                                return wm(OE > 1 && cU(SO), OE > 1 && Lb(rx.slice(0, OE - 1).concat({
                                    value: rx[OE - 2].type === " " ? "*" : ""
                                })).replace(aB, "$1"), Tb, OE < lo && hU(rx.slice(OE, lo)), lo < zC && hU(rx = rx.slice(lo)), lo < zC && Lb(rx))
                            }
                            SO.push(Tb)
                        }
                    return cU(SO)
                }
                function NX(rx, ZA) {
                    var Tb = ZA.length > 0
                      , lo = rx.length > 0
                      , zC = function(zC, uA, XH, OE, ZQ) {
                        var VX, ny, zO, Fc = 0, yI = "0", jG = zC && [], XF = [], yU = Us, dS = zC || lo && wB.find["TAG"]("*", ZQ), FW = QG += yU == null ? 1 : Math.random() || .1, nm = dS.length;
                        if (ZQ)
                            Us = uA === yi || uA || ZQ;
                        for (; yI !== nm && (VX = dS[yI]) != null; yI++) {
                            if (lo && VX) {
                                if (ny = 0,
                                !uA && VX.ownerDocument !== yi)
                                    SO(VX),
                                    XH = !Ze;
                                while (zO = rx[ny++])
                                    if (zO(VX, uA || yi, XH)) {
                                        OE.push(VX);
                                        break
                                    }
                                if (ZQ)
                                    QG = FW
                            }
                            if (Tb) {
                                if (VX = !zO && VX)
                                    Fc--;
                                if (zC)
                                    jG.push(VX)
                            }
                        }
                        if (Fc += yI,
                        Tb && yI !== Fc) {
                            ny = 0;
                            while (zO = ZA[ny++])
                                zO(jG, XF, uA, XH);
                            if (zC) {
                                if (Fc > 0)
                                    while (yI--)
                                        if (!(jG[yI] || XF[yI]))
                                            XF[yI] = vr.call(OE);
                                XF = Hm(XF)
                            }
                            if (Sg.apply(OE, XF),
                            ZQ && !zC && XF.length > 0 && Fc + ZA.length > 1)
                                nx.uniqueSort(OE)
                        }
                        if (ZQ)
                            QG = FW,
                            Us = yU;
                        return jG
                    };
                    return Tb ? yc(zC) : zC
                }
                if (Fy.prototype = wB.filters = wB.pseudos,
                wB.setFilters = new Fy,
                uA = nx.tokenize = function(rx, ZA) {
                    var Tb, lo, zC, uA, XH, OE, Us, ZQ = nm[rx + " "];
                    if (ZQ)
                        return ZA ? 0 : ZQ.slice(0);
                    XH = rx,
                    OE = [],
                    Us = wB.preFilter;
                    while (XH) {
                        if (!Tb || (lo = bb.exec(XH))) {
                            if (lo)
                                XH = XH.slice(lo[0].length) || XH;
                            OE.push(zC = [])
                        }
                        if (Tb = false,
                        lo = IG.exec(XH))
                            Tb = lo.shift(),
                            zC.push({
                                value: Tb,
                                type: lo[0].replace(aB, " ")
                            }),
                            XH = XH.slice(Tb.length);
                        for (uA in wB.filter)
                            if ((lo = vu[uA].exec(XH)) && (!Us[uA] || (lo = Us[uA](lo))))
                                Tb = lo.shift(),
                                zC.push({
                                    value: Tb,
                                    type: uA,
                                    matches: lo
                                }),
                                XH = XH.slice(Tb.length);
                        if (!Tb)
                            break
                    }
                    return ZA ? XH.length : XH ? nx.error(rx) : nm(rx, OE).slice(0)
                }
                ,
                XH = nx.compile = function(rx, ZA) {
                    var Tb, wB = [], lo = [], zC = uq[rx + " "];
                    if (!zC) {
                        if (!ZA)
                            ZA = uA(rx);
                        Tb = ZA.length;
                        while (Tb--)
                            if (zC = hU(ZA[Tb]),
                            zC[XF])
                                wB.push(zC);
                            else
                                lo.push(zC);
                        zC = uq(rx, NX(lo, wB)),
                        zC.selector = rx
                    }
                    return zC
                }
                ,
                OE = nx.select = function(rx, ZA, Tb, lo) {
                    var zC, OE, Us, ZQ, VX, SO = typeof rx === "function" && rx, yi = !lo && uA(rx = SO.selector || rx);
                    if (Tb = Tb || [],
                    yi.length === 1) {
                        if (OE = yi[0] = yi[0].slice(0),
                        OE.length > 2 && (Us = OE[0]).type === "ID" && ZA.nodeType === 9 && Ze && wB.relative[OE[1].type]) {
                            if (ZA = (wB.find["ID"](Us.matches[0].replace(nK, jT), ZA) || [])[0],
                            !ZA)
                                return Tb;
                            else if (SO)
                                ZA = ZA.parentNode;
                            rx = rx.slice(OE.shift().value.length)
                        }
                        zC = vu["needsContext"].test(rx) ? 0 : OE.length;
                        while (zC--) {
                            if (Us = OE[zC],
                            wB.relative[ZQ = Us.type])
                                break;
                            if (VX = wB.find[ZQ])
                                if (lo = VX(Us.matches[0].replace(nK, jT), eU.test(OE[0].type) && Zb(ZA.parentNode) || ZA)) {
                                    if (OE.splice(zC, 1),
                                    rx = lo.length && Lb(OE),
                                    !rx)
                                        return Sg.apply(Tb, lo),
                                        Tb;
                                    break
                                }
                        }
                    }
                    return (SO || XH(rx, yi))(lo, ZA, !Ze, Tb, !ZA || eU.test(rx) && Zb(ZA.parentNode) || ZA),
                    Tb
                }
                ,
                Tb.sortStable = XF.split("").sort(UI).join("") === XF,
                Tb.detectDuplicates = !!VX,
                SO(),
                Tb.sortDetached = bs((function(rx) {
                    return rx.compareDocumentPosition(yi.createElement("fieldset")) & 1
                }
                )),
                !bs((function(rx) {
                    return rx.innerHTML = "<a href='#'></a>",
                    rx.firstChild.getAttribute("href") === "#"
                }
                )))
                    et("type|href|height|width", (function(rx, ZA, Tb) {
                        if (!Tb)
                            return rx.getAttribute(ZA, ZA.toLowerCase() === "type" ? 1 : 2)
                    }
                    ));
                if (!Tb.attributes || !bs((function(rx) {
                    return rx.innerHTML = "<input/>",
                    rx.firstChild.setAttribute("value", ""),
                    rx.firstChild.getAttribute("value") === ""
                }
                )))
                    et("value", (function(rx, ZA, Tb) {
                        if (!Tb && rx.nodeName.toLowerCase() === "input")
                            return rx.defaultValue
                    }
                    ));
                if (!bs((function(rx) {
                    return rx.getAttribute("disabled") == null
                }
                )))
                    et(Re, (function(rx, ZA, Tb) {
                        var wB;
                        if (!Tb)
                            return rx[ZA] === true ? ZA.toLowerCase() : (wB = rx.getAttributeNode(ZA)) && wB.specified ? wB.value : null
                    }
                    ));
                return nx
            }(rx);
            yU.find = FW,
            yU.expr = FW.selectors,
            yU.expr[":"] = yU.expr.pseudos,
            yU.uniqueSort = yU.unique = FW.uniqueSort,
            yU.text = FW.getText,
            yU.isXMLDoc = FW.isXML,
            yU.contains = FW.contains,
            yU.escapeSelector = FW.escape;
            var nm = function(rx, ZA, Tb) {
                var wB = []
                  , lo = Tb !== void 0;
                while ((rx = rx[ZA]) && rx.nodeType !== 9)
                    if (rx.nodeType === 1) {
                        if (lo && yU(rx).is(Tb))
                            break;
                        wB.push(rx)
                    }
                return wB
            }
              , uq = function(rx, ZA) {
                for (var Tb = []; rx; rx = rx.nextSibling)
                    if (rx.nodeType === 1 && rx !== ZA)
                        Tb.push(rx);
                return Tb
            }
              , EZ = yU.expr.match.needsContext;
            function UI(rx, ZA) {
                return rx.nodeName && rx.nodeName.toLowerCase() === ZA.toLowerCase()
            }
            var iO = /^<([a-z][^\/\0>:\x20\t\r\n\f]*)[\x20\t\r\n\f]*\/?>(?:<\/\1>|)$/i;
            function TE(rx, ZA, Tb) {
                if (Ze(ZA))
                    return yU.grep(rx, (function(rx, wB) {
                        return !!ZA.call(rx, wB, rx) !== Tb
                    }
                    ));
                if (ZA.nodeType)
                    return yU.grep(rx, (function(rx) {
                        return rx === ZA !== Tb
                    }
                    ));
                if (typeof ZA !== "string")
                    return yU.grep(rx, (function(rx) {
                        return OE.call(ZA, rx) > -1 !== Tb
                    }
                    ));
                return yU.filter(ZA, rx, Tb)
            }
            yU.filter = function(rx, ZA, Tb) {
                var wB = ZA[0];
                if (Tb)
                    rx = ":not(" + rx + ")";
                if (ZA.length === 1 && wB.nodeType === 1)
                    return yU.find.matchesSelector(wB, rx) ? [wB] : [];
                return yU.find.matches(rx, yU.grep(ZA, (function(rx) {
                    return rx.nodeType === 1
                }
                )))
            }
            ,
            yU.fn.extend({
                find: function(rx) {
                    var ZA, Tb, wB = this.length, lo = this;
                    if (typeof rx !== "string")
                        return this.pushStack(yU(rx).filter((function() {
                            for (ZA = 0; ZA < wB; ZA++)
                                if (yU.contains(lo[ZA], this))
                                    return true
                        }
                        )));
                    for (Tb = this.pushStack([]),
                    ZA = 0; ZA < wB; ZA++)
                        yU.find(rx, lo[ZA], Tb);
                    return wB > 1 ? yU.uniqueSort(Tb) : Tb
                },
                filter: function(rx) {
                    return this.pushStack(TE(this, rx || [], false))
                },
                not: function(rx) {
                    return this.pushStack(TE(this, rx || [], true))
                },
                is: function(rx) {
                    return !!TE(this, typeof rx === "string" && EZ.test(rx) ? yU(rx) : rx || [], false).length
                }
            });
            var vr, QW = /^(?:\s*(<[\w\W]+>)[^>]*|#([\w-]+))$/, Sg = yU.fn.init = function(rx, ZA, Tb) {
                var lo, zC;
                if (!rx)
                    return this;
                if (Tb = Tb || vr,
                typeof rx === "string") {
                    if (rx[0] === "<" && rx[rx.length - 1] === ">" && rx.length >= 3)
                        lo = [null, rx, null];
                    else
                        lo = QW.exec(rx);
                    if (lo && (lo[1] || !ZA))
                        if (lo[1]) {
                            if (ZA = ZA instanceof yU ? ZA[0] : ZA,
                            yU.merge(this, yU.parseHTML(lo[1], ZA && ZA.nodeType ? ZA.ownerDocument || ZA : wB, true)),
                            iO.test(lo[1]) && yU.isPlainObject(ZA))
                                for (lo in ZA)
                                    if (Ze(this[lo]))
                                        this[lo](ZA[lo]);
                                    else
                                        this.attr(lo, ZA[lo]);
                            return this
                        } else {
                            if (zC = wB.getElementById(lo[2]),
                            zC)
                                this[0] = zC,
                                this.length = 1;
                            return this
                        }
                    else if (!ZA || ZA.jquery)
                        return (ZA || Tb).find(rx);
                    else
                        return this.constructor(ZA).find(rx)
                } else if (rx.nodeType)
                    return this[0] = rx,
                    this.length = 1,
                    this;
                else if (Ze(rx))
                    return Tb.ready !== void 0 ? Tb.ready(rx) : rx(yU);
                return yU.makeArray(rx, this)
            }
            ;
            Sg.prototype = yU.fn,
            vr = yU(wB);
            var YA = /^(?:parents|prev(?:Until|All))/
              , ij = {
                children: true,
                contents: true,
                next: true,
                prev: true
            };
            function Re(rx, ZA) {
                while ((rx = rx[ZA]) && rx.nodeType !== 1)
                    ;
                return rx
            }
            yU.fn.extend({
                has: function(rx) {
                    var ZA = yU(rx, this)
                      , Tb = ZA.length;
                    return this.filter((function() {
                        for (var rx = 0; rx < Tb; rx++)
                            if (yU.contains(this, ZA[rx]))
                                return true
                    }
                    ))
                },
                closest: function(rx, ZA) {
                    var Tb, wB = 0, lo = this.length, zC = [], uA = typeof rx !== "string" && yU(rx);
                    if (!EZ.test(rx))
                        for (; wB < lo; wB++)
                            for (Tb = this[wB]; Tb && Tb !== ZA; Tb = Tb.parentNode)
                                if (Tb.nodeType < 11 && (uA ? uA.index(Tb) > -1 : Tb.nodeType === 1 && yU.find.matchesSelector(Tb, rx))) {
                                    zC.push(Tb);
                                    break
                                }
                    return this.pushStack(zC.length > 1 ? yU.uniqueSort(zC) : zC)
                },
                index: function(rx) {
                    if (!rx)
                        return this[0] && this[0].parentNode ? this.first().prevAll().length : -1;
                    if (typeof rx === "string")
                        return OE.call(yU(rx), this[0]);
                    return OE.call(this, rx.jquery ? rx[0] : rx)
                },
                add: function(rx, ZA) {
                    return this.pushStack(yU.uniqueSort(yU.merge(this.get(), yU(rx, ZA))))
                },
                addBack: function(rx) {
                    return this.add(rx == null ? this.prevObject : this.prevObject.filter(rx))
                }
            }),
            yU.each({
                parent: function(rx) {
                    var ZA = rx.parentNode;
                    return ZA && ZA.nodeType !== 11 ? ZA : null
                },
                parents: function(rx) {
                    return nm(rx, "parentNode")
                },
                parentsUntil: function(rx, ZA, Tb) {
                    return nm(rx, "parentNode", Tb)
                },
                next: function(rx) {
                    return Re(rx, "nextSibling")
                },
                prev: function(rx) {
                    return Re(rx, "previousSibling")
                },
                nextAll: function(rx) {
                    return nm(rx, "nextSibling")
                },
                prevAll: function(rx) {
                    return nm(rx, "previousSibling")
                },
                nextUntil: function(rx, ZA, Tb) {
                    return nm(rx, "nextSibling", Tb)
                },
                prevUntil: function(rx, ZA, Tb) {
                    return nm(rx, "previousSibling", Tb)
                },
                siblings: function(rx) {
                    return uq((rx.parentNode || {}).firstChild, rx)
                },
                children: function(rx) {
                    return uq(rx.firstChild)
                },
                contents: function(rx) {
                    if (typeof rx.contentDocument !== "undefined")
                        return rx.contentDocument;
                    if (UI(rx, "template"))
                        rx = rx.content || rx;
                    return yU.merge([], rx.childNodes)
                }
            }, (function(rx, ZA) {
                yU.fn[rx] = function(Tb, wB) {
                    var lo = yU.map(this, ZA, Tb);
                    if (rx.slice(-5) !== "Until")
                        wB = Tb;
                    if (wB && typeof wB === "string")
                        lo = yU.filter(wB, lo);
                    if (this.length > 1) {
                        if (!ij[rx])
                            yU.uniqueSort(lo);
                        if (YA.test(rx))
                            lo.reverse()
                    }
                    return this.pushStack(lo)
                }
            }
            ));
            var oD = /[^\x20\t\r\n\f]+/g;
            function sy(rx) {
                var ZA = {};
                return yU.each(rx.match(oD) || [], (function(rx, Tb) {
                    ZA[Tb] = true
                }
                )),
                ZA
            }
            function Ex(rx) {
                return rx
            }
            function d(rx) {
                throw rx
            }
            function KO(rx, ZA, Tb, wB) {
                var lo;
                try {
                    if (rx && Ze(lo = rx.promise))
                        lo.call(rx).done(ZA).fail(Tb);
                    else if (rx && Ze(lo = rx.then))
                        lo.call(rx, ZA, Tb);
                    else
                        ZA.apply(void 0, [rx].slice(wB))
                } catch (rx) {
                    Tb.apply(void 0, [rx])
                }
            }
            yU.Callbacks = function(rx) {
                rx = typeof rx === "string" ? sy(rx) : yU.extend({}, rx);
                var ZA, Tb, wB, lo, zC = [], uA = [], XH = -1, OE = function() {
                    for (lo = lo || rx.once,
                    wB = ZA = true; uA.length; XH = -1) {
                        Tb = uA.shift();
                        while (++XH < zC.length)
                            if (zC[XH].apply(Tb[0], Tb[1]) === false && rx.stopOnFalse)
                                XH = zC.length,
                                Tb = false
                    }
                    if (!rx.memory)
                        Tb = false;
                    if (ZA = false,
                    lo)
                        if (Tb)
                            zC = [];
                        else
                            zC = ""
                }, Us = {
                    add: function() {
                        if (zC) {
                            if (Tb && !ZA)
                                XH = zC.length - 1,
                                uA.push(Tb);
                            if (function ZA(Tb) {
                                yU.each(Tb, (function(Tb, wB) {
                                    if (Ze(wB)) {
                                        if (!rx.unique || !Us.has(wB))
                                            zC.push(wB)
                                    } else if (wB && wB.length && jG(wB) !== "string")
                                        ZA(wB)
                                }
                                ))
                            }(arguments),
                            Tb && !ZA)
                                OE()
                        }
                        return this
                    },
                    remove: function() {
                        return yU.each(arguments, (function(rx, ZA) {
                            var Tb;
                            while ((Tb = yU.inArray(ZA, zC, Tb)) > -1)
                                if (zC.splice(Tb, 1),
                                Tb <= XH)
                                    XH--
                        }
                        )),
                        this
                    },
                    has: function(rx) {
                        return rx ? yU.inArray(rx, zC) > -1 : zC.length > 0
                    },
                    empty: function() {
                        if (zC)
                            zC = [];
                        return this
                    },
                    disable: function() {
                        return lo = uA = [],
                        zC = Tb = "",
                        this
                    },
                    disabled: function() {
                        return !zC
                    },
                    lock: function() {
                        if (lo = uA = [],
                        !Tb && !ZA)
                            zC = Tb = "";
                        return this
                    },
                    locked: function() {
                        return !!lo
                    },
                    fireWith: function(rx, Tb) {
                        if (!lo)
                            if (Tb = Tb || [],
                            Tb = [rx, Tb.slice ? Tb.slice() : Tb],
                            uA.push(Tb),
                            !ZA)
                                OE();
                        return this
                    },
                    fire: function() {
                        return Us.fireWith(this, arguments),
                        this
                    },
                    fired: function() {
                        return !!wB
                    }
                };
                return Us
            }
            ,
            yU.extend({
                Deferred: function(ZA) {
                    var Tb = [["notify", "progress", yU.Callbacks("memory"), yU.Callbacks("memory"), 2], ["resolve", "done", yU.Callbacks("once memory"), yU.Callbacks("once memory"), 0, "resolved"], ["reject", "fail", yU.Callbacks("once memory"), yU.Callbacks("once memory"), 1, "rejected"]]
                      , wB = "pending"
                      , lo = {
                        state: function() {
                            return wB
                        },
                        always: function() {
                            return zC.done(arguments).fail(arguments),
                            this
                        },
                        catch: function(rx) {
                            return lo.then(null, rx)
                        },
                        pipe: function() {
                            var rx = arguments;
                            return yU.Deferred((function(ZA) {
                                yU.each(Tb, (function(Tb, wB) {
                                    var lo = Ze(rx[wB[4]]) && rx[wB[4]];
                                    zC[wB[1]]((function() {
                                        var rx = lo && lo.apply(this, arguments);
                                        if (rx && Ze(rx.promise))
                                            rx.promise().progress(ZA.notify).done(ZA.resolve).fail(ZA.reject);
                                        else
                                            ZA[wB[0] + "With"](this, lo ? [rx] : arguments)
                                    }
                                    ))
                                }
                                )),
                                rx = null
                            }
                            )).promise()
                        },
                        then: function(ZA, wB, lo) {
                            var zC = 0;
                            function uA(ZA, Tb, wB, lo) {
                                return function() {
                                    var XH = this
                                      , OE = arguments
                                      , Us = function() {
                                        var rx, Us;
                                        if (ZA < zC)
                                            return;
                                        if (rx = wB.apply(XH, OE),
                                        rx === Tb.promise())
                                            throw new TypeError("Thenable self-resolution");
                                        if (Us = rx && (typeof rx === "object" || typeof rx === "function") && rx.then,
                                        Ze(Us))
                                            if (lo)
                                                Us.call(rx, uA(zC, Tb, Ex, lo), uA(zC, Tb, d, lo));
                                            else
                                                zC++,
                                                Us.call(rx, uA(zC, Tb, Ex, lo), uA(zC, Tb, d, lo), uA(zC, Tb, Ex, Tb.notifyWith));
                                        else {
                                            if (wB !== Ex)
                                                XH = void 0,
                                                OE = [rx];
                                            (lo || Tb.resolveWith)(XH, OE)
                                        }
                                    }
                                      , ZQ = lo ? Us : function() {
                                        try {
                                            Us()
                                        } catch (rx) {
                                            if (yU.Deferred.exceptionHook)
                                                yU.Deferred.exceptionHook(rx, ZQ.stackTrace);
                                            if (ZA + 1 >= zC) {
                                                if (wB !== d)
                                                    XH = void 0,
                                                    OE = [rx];
                                                Tb.rejectWith(XH, OE)
                                            }
                                        }
                                    }
                                    ;
                                    if (ZA)
                                        ZQ();
                                    else {
                                        if (yU.Deferred.getStackHook)
                                            ZQ.stackTrace = yU.Deferred.getStackHook();
                                        rx.setTimeout(ZQ)
                                    }
                                }
                            }
                            return yU.Deferred((function(rx) {
                                Tb[0][3].add(uA(0, rx, Ze(lo) ? lo : Ex, rx.notifyWith)),
                                Tb[1][3].add(uA(0, rx, Ze(ZA) ? ZA : Ex)),
                                Tb[2][3].add(uA(0, rx, Ze(wB) ? wB : d))
                            }
                            )).promise()
                        },
                        promise: function(rx) {
                            return rx != null ? yU.extend(rx, lo) : lo
                        }
                    }
                      , zC = {};
                    if (yU.each(Tb, (function(rx, ZA) {
                        var uA = ZA[2]
                          , XH = ZA[5];
                        if (lo[ZA[1]] = uA.add,
                        XH)
                            uA.add((function() {
                                wB = XH
                            }
                            ), Tb[3 - rx][2].disable, Tb[3 - rx][3].disable, Tb[0][2].lock, Tb[0][3].lock);
                        uA.add(ZA[3].fire),
                        zC[ZA[0]] = function() {
                            return zC[ZA[0] + "With"](this === zC ? void 0 : this, arguments),
                            this
                        }
                        ,
                        zC[ZA[0] + "With"] = uA.fireWith
                    }
                    )),
                    lo.promise(zC),
                    ZA)
                        ZA.call(zC, zC);
                    return zC
                },
                when: function(rx) {
                    var ZA = arguments.length
                      , Tb = ZA
                      , wB = Array(Tb)
                      , lo = zC.call(arguments)
                      , uA = yU.Deferred()
                      , XH = function(rx) {
                        return function(Tb) {
                            if (wB[rx] = this,
                            lo[rx] = arguments.length > 1 ? zC.call(arguments) : Tb,
                            !--ZA)
                                uA.resolveWith(wB, lo)
                        }
                    };
                    if (ZA <= 1)
                        if (KO(rx, uA.done(XH(Tb)).resolve, uA.reject, !ZA),
                        uA.state() === "pending" || Ze(lo[Tb] && lo[Tb].then))
                            return uA.then();
                    while (Tb--)
                        KO(lo[Tb], XH(Tb), uA.reject);
                    return uA.promise()
                }
            });
            var aB = /^(Eval|Internal|Range|Reference|Syntax|Type|URI)Error$/;
            yU.Deferred.exceptionHook = function(ZA, Tb) {
                if (rx.console && rx.console.warn && ZA && aB.test(ZA.name))
                    rx.console.warn("jQuery.Deferred exception: " + ZA.message, ZA.stack, Tb)
            }
            ,
            yU.readyException = function(ZA) {
                rx.setTimeout((function() {
                    throw ZA
                }
                ))
            }
            ;
            var bb = yU.Deferred();
            function IG() {
                wB.removeEventListener("DOMContentLoaded", IG),
                rx.removeEventListener("load", IG),
                yU.ready()
            }
            if (yU.fn.ready = function(rx) {
                return bb.then(rx).catch((function(rx) {
                    yU.readyException(rx)
                }
                )),
                this
            }
            ,
            yU.extend({
                isReady: false,
                readyWait: 1,
                ready: function(rx) {
                    if (rx === true ? --yU.readyWait : yU.isReady)
                        return;
                    if (yU.isReady = true,
                    rx !== true && --yU.readyWait > 0)
                        return;
                    bb.resolveWith(wB, [yU])
                }
            }),
            yU.ready.then = bb.then,
            wB.readyState === "complete" || wB.readyState !== "loading" && !wB.documentElement.doScroll)
                rx.setTimeout(yU.ready);
            else
                wB.addEventListener("DOMContentLoaded", IG),
                rx.addEventListener("load", IG);
            var wJ = function(rx, ZA, Tb, wB, lo, zC, uA) {
                var XH = 0
                  , OE = rx.length
                  , Us = Tb == null;
                if (jG(Tb) === "object")
                    for (XH in lo = true,
                    Tb)
                        wJ(rx, ZA, XH, Tb[XH], true, zC, uA);
                else if (wB !== void 0) {
                    if (lo = true,
                    !Ze(wB))
                        uA = true;
                    if (Us)
                        if (uA)
                            ZA.call(rx, wB),
                            ZA = null;
                        else
                            Us = ZA,
                            ZA = function(rx, ZA, Tb) {
                                return Us.call(yU(rx), Tb)
                            }
                            ;
                    if (ZA)
                        for (; XH < OE; XH++)
                            ZA(rx[XH], Tb, uA ? wB : wB.call(rx[XH], XH, ZA(rx[XH], Tb)))
                }
                if (lo)
                    return rx;
                if (Us)
                    return ZA.call(rx);
                return OE ? ZA(rx[0], Tb) : zC
            }
              , JE = /^-ms-/
              , YU = /-([a-z])/g;
            function vu(rx, ZA) {
                return ZA.toUpperCase()
            }
            function xh(rx) {
                return rx.replace(JE, "ms-").replace(YU, vu)
            }
            var uH = function(rx) {
                return rx.nodeType === 1 || rx.nodeType === 9 || !+rx.nodeType
            };
            function NF() {
                this.expando = yU.expando + NF.uid++
            }
            NF.uid = 1,
            NF.prototype = {
                cache: function(rx) {
                    var ZA = rx[this.expando];
                    if (!ZA)
                        if (ZA = {},
                        uH(rx))
                            if (rx.nodeType)
                                rx[this.expando] = ZA;
                            else
                                Object.defineProperty(rx, this.expando, {
                                    value: ZA,
                                    configurable: true
                                });
                    return ZA
                },
                set: function(rx, ZA, Tb) {
                    var wB, lo = this.cache(rx);
                    if (typeof ZA === "string")
                        lo[xh(ZA)] = Tb;
                    else
                        for (wB in ZA)
                            lo[xh(wB)] = ZA[wB];
                    return lo
                },
                get: function(rx, ZA) {
                    return ZA === void 0 ? this.cache(rx) : rx[this.expando] && rx[this.expando][xh(ZA)]
                },
                access: function(rx, ZA, Tb) {
                    if (ZA === void 0 || ZA && typeof ZA === "string" && Tb === void 0)
                        return this.get(rx, ZA);
                    return this.set(rx, ZA, Tb),
                    Tb !== void 0 ? Tb : ZA
                },
                remove: function(rx, ZA) {
                    var Tb, wB = rx[this.expando];
                    if (wB === void 0)
                        return;
                    if (ZA !== void 0) {
                        if (Array.isArray(ZA))
                            ZA = ZA.map(xh);
                        else
                            ZA = xh(ZA),
                            ZA = ZA in wB ? [ZA] : ZA.match(oD) || [];
                        Tb = ZA.length;
                        while (Tb--)
                            delete wB[ZA[Tb]]
                    }
                    if (ZA === void 0 || yU.isEmptyObject(wB))
                        if (rx.nodeType)
                            rx[this.expando] = void 0;
                        else
                            delete rx[this.expando]
                },
                hasData: function(rx) {
                    var ZA = rx[this.expando];
                    return ZA !== void 0 && !yU.isEmptyObject(ZA)
                }
            };
            var tv = new NF
              , Xo = new NF
              , eU = /^(?:\{[\w\W]*\}|\[[\w\W]*\])$/
              , nK = /[A-Z]/g;
            function jT(rx) {
                if (rx === "true")
                    return true;
                if (rx === "false")
                    return false;
                if (rx === "null")
                    return null;
                if (rx === +rx + "")
                    return +rx;
                if (eU.test(rx))
                    return JSON.parse(rx);
                return rx
            }
            function ba(rx, ZA, Tb) {
                var wB;
                if (Tb === void 0 && rx.nodeType === 1)
                    if (wB = "data-" + ZA.replace(nK, "-$&").toLowerCase(),
                    Tb = rx.getAttribute(wB),
                    typeof Tb === "string") {
                        try {
                            Tb = jT(Tb)
                        } catch (rx) {}
                        Xo.set(rx, ZA, Tb)
                    } else
                        Tb = void 0;
                return Tb
            }
            yU.extend({
                hasData: function(rx) {
                    return Xo.hasData(rx) || tv.hasData(rx)
                },
                data: function(rx, ZA, Tb) {
                    return Xo.access(rx, ZA, Tb)
                },
                removeData: function(rx, ZA) {
                    Xo.remove(rx, ZA)
                },
                _data: function(rx, ZA, Tb) {
                    return tv.access(rx, ZA, Tb)
                },
                _removeData: function(rx, ZA) {
                    tv.remove(rx, ZA)
                }
            }),
            yU.fn.extend({
                data: function(rx, ZA) {
                    var Tb, wB, lo, zC = this[0], uA = zC && zC.attributes;
                    if (rx === void 0) {
                        if (this.length)
                            if (lo = Xo.get(zC),
                            zC.nodeType === 1 && !tv.get(zC, "hasDataAttrs")) {
                                Tb = uA.length;
                                while (Tb--)
                                    if (uA[Tb])
                                        if (wB = uA[Tb].name,
                                        wB.indexOf("data-") === 0)
                                            wB = xh(wB.slice(5)),
                                            ba(zC, wB, lo[wB]);
                                tv.set(zC, "hasDataAttrs", true)
                            }
                        return lo
                    }
                    if (typeof rx === "object")
                        return this.each((function() {
                            Xo.set(this, rx)
                        }
                        ));
                    return wJ(this, (function(ZA) {
                        var Tb;
                        if (zC && ZA === void 0) {
                            if (Tb = Xo.get(zC, rx),
                            Tb !== void 0)
                                return Tb;
                            if (Tb = ba(zC, rx),
                            Tb !== void 0)
                                return Tb;
                            return
                        }
                        this.each((function() {
                            Xo.set(this, rx, ZA)
                        }
                        ))
                    }
                    ), null, ZA, arguments.length > 1, null, true)
                },
                removeData: function(rx) {
                    return this.each((function() {
                        Xo.remove(this, rx)
                    }
                    ))
                }
            }),
            yU.extend({
                queue: function(rx, ZA, Tb) {
                    var wB;
                    if (rx) {
                        if (ZA = (ZA || "fx") + "queue",
                        wB = tv.get(rx, ZA),
                        Tb)
                            if (!wB || Array.isArray(Tb))
                                wB = tv.access(rx, ZA, yU.makeArray(Tb));
                            else
                                wB.push(Tb);
                        return wB || []
                    }
                },
                dequeue: function(rx, ZA) {
                    ZA = ZA || "fx";
                    var Tb = yU.queue(rx, ZA)
                      , wB = Tb.length
                      , lo = Tb.shift()
                      , zC = yU._queueHooks(rx, ZA)
                      , uA = function() {
                        yU.dequeue(rx, ZA)
                    };
                    if (lo === "inprogress")
                        lo = Tb.shift(),
                        wB--;
                    if (lo) {
                        if (ZA === "fx")
                            Tb.unshift("inprogress");
                        delete zC.stop,
                        lo.call(rx, uA, zC)
                    }
                    if (!wB && zC)
                        zC.empty.fire()
                },
                _queueHooks: function(rx, ZA) {
                    var Tb = ZA + "queueHooks";
                    return tv.get(rx, Tb) || tv.access(rx, Tb, {
                        empty: yU.Callbacks("once memory").add((function() {
                            tv.remove(rx, [ZA + "queue", Tb])
                        }
                        ))
                    })
                }
            }),
            yU.fn.extend({
                queue: function(rx, ZA) {
                    var Tb = 2;
                    if (typeof rx !== "string")
                        ZA = rx,
                        rx = "fx",
                        Tb--;
                    if (arguments.length < Tb)
                        return yU.queue(this[0], rx);
                    return ZA === void 0 ? this : this.each((function() {
                        var Tb = yU.queue(this, rx, ZA);
                        if (yU._queueHooks(this, rx),
                        rx === "fx" && Tb[0] !== "inprogress")
                            yU.dequeue(this, rx)
                    }
                    ))
                },
                dequeue: function(rx) {
                    return this.each((function() {
                        yU.dequeue(this, rx)
                    }
                    ))
                },
                clearQueue: function(rx) {
                    return this.queue(rx || "fx", [])
                },
                promise: function(rx, ZA) {
                    var Tb, wB = 1, lo = yU.Deferred(), zC = this, uA = this.length, XH = function() {
                        if (!--wB)
                            lo.resolveWith(zC, [zC])
                    };
                    if (typeof rx !== "string")
                        ZA = rx,
                        rx = void 0;
                    rx = rx || "fx";
                    while (uA--)
                        if (Tb = tv.get(zC[uA], rx + "queueHooks"),
                        Tb && Tb.empty)
                            wB++,
                            Tb.empty.add(XH);
                    return XH(),
                    lo.promise(ZA)
                }
            });
            var fL = /[+-]?(?:\d*\.|)\d+(?:[eE][+-]?\d+|)/.source
              , ar = new RegExp("^(?:([+-])=|)(" + fL + ")([a-z%]*)$","i")
              , GW = ["Top", "Right", "Bottom", "Left"]
              , nx = wB.documentElement
              , hQ = function(rx) {
                return yU.contains(rx.ownerDocument, rx)
            }
              , yc = {
                composed: true
            };
            if (nx.getRootNode)
                hQ = function(rx) {
                    return yU.contains(rx.ownerDocument, rx) || rx.getRootNode(yc) === rx.ownerDocument
                }
                ;
            var bs = function(rx, ZA) {
                return rx = ZA || rx,
                rx.style.display === "none" || rx.style.display === "" && hQ(rx) && yU.css(rx, "display") === "none"
            }
              , et = function(rx, ZA, Tb, wB) {
                var lo, zC, uA = {};
                for (zC in ZA)
                    uA[zC] = rx.style[zC],
                    rx.style[zC] = ZA[zC];
                for (zC in lo = Tb.apply(rx, wB || []),
                ZA)
                    rx.style[zC] = uA[zC];
                return lo
            };
            function yJ(rx, ZA, Tb, wB) {
                var lo, zC, uA = 20, XH = wB ? function() {
                    return wB.cur()
                }
                : function() {
                    return yU.css(rx, ZA, "")
                }
                , OE = XH(), Us = Tb && Tb[3] || (yU.cssNumber[ZA] ? "" : "px"), ZQ = rx.nodeType && (yU.cssNumber[ZA] || Us !== "px" && +OE) && ar.exec(yU.css(rx, ZA));
                if (ZQ && ZQ[3] !== Us) {
                    OE /= 2,
                    Us = Us || ZQ[3],
                    ZQ = +OE || 1;
                    while (uA--) {
                        if (yU.style(rx, ZA, ZQ + Us),
                        (1 - zC) * (1 - (zC = XH() / OE || .5)) <= 0)
                            uA = 0;
                        ZQ /= zC
                    }
                    ZQ *= 2,
                    yU.style(rx, ZA, ZQ + Us),
                    Tb = Tb || []
                }
                if (Tb)
                    if (ZQ = +ZQ || +OE || 0,
                    lo = Tb[1] ? ZQ + (Tb[1] + 1) * Tb[2] : +Tb[2],
                    wB)
                        wB.unit = Us,
                        wB.start = ZQ,
                        wB.end = lo;
                return lo
            }
            var BZ = {};
            function cb(rx) {
                var ZA, Tb = rx.ownerDocument, wB = rx.nodeName, lo = BZ[wB];
                if (lo)
                    return lo;
                if (ZA = Tb.body.appendChild(Tb.createElement(wB)),
                lo = yU.css(ZA, "display"),
                ZA.parentNode.removeChild(ZA),
                lo === "none")
                    lo = "block";
                return BZ[wB] = lo,
                lo
            }
            function Jt(rx, ZA) {
                for (var Tb, wB, lo = [], zC = 0, uA = rx.length; zC < uA; zC++) {
                    if (wB = rx[zC],
                    !wB.style)
                        continue;
                    if (Tb = wB.style.display,
                    ZA) {
                        if (Tb === "none")
                            if (lo[zC] = tv.get(wB, "display") || null,
                            !lo[zC])
                                wB.style.display = "";
                        if (wB.style.display === "" && bs(wB))
                            lo[zC] = cb(wB)
                    } else if (Tb !== "none")
                        lo[zC] = "none",
                        tv.set(wB, "display", Tb)
                }
                for (zC = 0; zC < uA; zC++)
                    if (lo[zC] != null)
                        rx[zC].style.display = lo[zC];
                return rx
            }
            yU.fn.extend({
                show: function() {
                    return Jt(this, true)
                },
                hide: function() {
                    return Jt(this)
                },
                toggle: function(rx) {
                    if (typeof rx === "boolean")
                        return rx ? this.show() : this.hide();
                    return this.each((function() {
                        if (bs(this))
                            yU(this).show();
                        else
                            yU(this).hide()
                    }
                    ))
                }
            });
            var Kd = /^(?:checkbox|radio)$/i
              , Zb = /<([a-z][^\/\0>\x20\t\r\n\f]*)/i
              , Fy = /^$|^module$|\/(?:java|ecma)script/i
              , Lb = {
                option: [1, "<select multiple='multiple'>", "</select>"],
                thead: [1, "<table>", "</table>"],
                col: [2, "<table><colgroup>", "</colgroup></table>"],
                tr: [2, "<table><tbody>", "</tbody></table>"],
                td: [3, "<table><tbody><tr>", "</tr></tbody></table>"],
                _default: [0, "", ""]
            };
            function ON(rx, ZA) {
                var Tb;
                if (typeof rx.getElementsByTagName !== "undefined")
                    Tb = rx.getElementsByTagName(ZA || "*");
                else if (typeof rx.querySelectorAll !== "undefined")
                    Tb = rx.querySelectorAll(ZA || "*");
                else
                    Tb = [];
                if (ZA === void 0 || ZA && UI(rx, ZA))
                    return yU.merge([rx], Tb);
                return Tb
            }
            function cU(rx, ZA) {
                for (var Tb = 0, wB = rx.length; Tb < wB; Tb++)
                    tv.set(rx[Tb], "globalEval", !ZA || tv.get(ZA[Tb], "globalEval"))
            }
            Lb.optgroup = Lb.option,
            Lb.tbody = Lb.tfoot = Lb.colgroup = Lb.caption = Lb.thead,
            Lb.th = Lb.td;
            var iE = /<|&#?\w+;/, Hm, wm, hU;
            function NX(rx, ZA, Tb, wB, lo) {
                for (var zC, uA, XH, OE, Us, ZQ, VX = ZA.createDocumentFragment(), SO = [], yi = 0, ny = rx.length; yi < ny; yi++)
                    if (zC = rx[yi],
                    zC || zC === 0)
                        if (jG(zC) === "object")
                            yU.merge(SO, zC.nodeType ? [zC] : zC);
                        else if (!iE.test(zC))
                            SO.push(ZA.createTextNode(zC));
                        else {
                            uA = uA || VX.appendChild(ZA.createElement("div")),
                            XH = (Zb.exec(zC) || ["", ""])[1].toLowerCase(),
                            OE = Lb[XH] || Lb._default,
                            uA.innerHTML = OE[1] + yU.htmlPrefilter(zC) + OE[2],
                            ZQ = OE[0];
                            while (ZQ--)
                                uA = uA.lastChild;
                            yU.merge(SO, uA.childNodes),
                            uA = VX.firstChild,
                            uA.textContent = ""
                        }
                VX.textContent = "",
                yi = 0;
                while (zC = SO[yi++]) {
                    if (wB && yU.inArray(zC, wB) > -1) {
                        if (lo)
                            lo.push(zC);
                        continue
                    }
                    if (Us = hQ(zC),
                    uA = ON(VX.appendChild(zC), "script"),
                    Us)
                        cU(uA);
                    if (Tb) {
                        ZQ = 0;
                        while (zC = uA[ZQ++])
                            if (Fy.test(zC.type || ""))
                                Tb.push(zC)
                    }
                }
                return VX
            }
            Hm = wB.createDocumentFragment(),
            wm = Hm.appendChild(wB.createElement("div")),
            hU = wB.createElement("input"),
            hU.setAttribute("type", "radio"),
            hU.setAttribute("checked", "checked"),
            hU.setAttribute("name", "t"),
            wm.appendChild(hU),
            ny.checkClone = wm.cloneNode(true).cloneNode(true).lastChild.checked,
            wm.innerHTML = "<textarea>x</textarea>",
            ny.noCloneChecked = !!wm.cloneNode(true).lastChild.defaultValue;
            var Lq = /^key/
              , Ag = /^(?:mouse|pointer|contextmenu|drag|drop)|click/
              , Jc = /^([^.]*)(?:\.(.+)|)/;
            function as() {
                return true
            }
            function qj() {
                return false
            }
            function pA(rx, ZA) {
                return rx === ep() === (ZA === "focus")
            }
            function ep() {
                try {
                    return wB.activeElement
                } catch (rx) {}
            }
            function Fa(rx, ZA, Tb, wB, lo, zC) {
                var uA, XH;
                if (typeof ZA === "object") {
                    if (typeof Tb !== "string")
                        wB = wB || Tb,
                        Tb = void 0;
                    for (XH in ZA)
                        Fa(rx, XH, Tb, wB, ZA[XH], zC);
                    return rx
                }
                if (wB == null && lo == null)
                    lo = Tb,
                    wB = Tb = void 0;
                else if (lo == null)
                    if (typeof Tb === "string")
                        lo = wB,
                        wB = void 0;
                    else
                        lo = wB,
                        wB = Tb,
                        Tb = void 0;
                if (lo === false)
                    lo = qj;
                else if (!lo)
                    return rx;
                if (zC === 1)
                    uA = lo,
                    lo = function(rx) {
                        return yU().off(rx),
                        uA.apply(this, arguments)
                    }
                    ,
                    lo.guid = uA.guid || (uA.guid = yU.guid++);
                return rx.each((function() {
                    yU.event.add(this, ZA, lo, wB, Tb)
                }
                ))
            }
            function ry(rx, ZA, Tb) {
                if (!Tb) {
                    if (tv.get(rx, ZA) === void 0)
                        yU.event.add(rx, ZA, as);
                    return
                }
                tv.set(rx, ZA, false),
                yU.event.add(rx, ZA, {
                    namespace: false,
                    handler: function(rx) {
                        var wB, lo, uA = tv.get(this, ZA);
                        if (rx.isTrigger & 1 && this[ZA]) {
                            if (!uA.length) {
                                if (uA = zC.call(arguments),
                                tv.set(this, ZA, uA),
                                wB = Tb(this, ZA),
                                this[ZA](),
                                lo = tv.get(this, ZA),
                                uA !== lo || wB)
                                    tv.set(this, ZA, false);
                                else
                                    lo = {};
                                if (uA !== lo)
                                    return rx.stopImmediatePropagation(),
                                    rx.preventDefault(),
                                    lo.value
                            } else if ((yU.event.special[ZA] || {}).delegateType)
                                rx.stopPropagation()
                        } else if (uA.length)
                            tv.set(this, ZA, {
                                value: yU.event.trigger(yU.extend(uA[0], yU.Event.prototype), uA.slice(1), this)
                            }),
                            rx.stopImmediatePropagation()
                    }
                })
            }
            yU.event = {
                global: {},
                add: function(rx, ZA, Tb, wB, lo) {
                    var zC, uA, XH, OE, Us, ZQ, VX, SO, yi, ny, Ze, zO = tv.get(rx);
                    if (!zO)
                        return;
                    if (Tb.handler)
                        zC = Tb,
                        Tb = zC.handler,
                        lo = zC.selector;
                    if (lo)
                        yU.find.matchesSelector(nx, lo);
                    if (!Tb.guid)
                        Tb.guid = yU.guid++;
                    if (!(OE = zO.events))
                        OE = zO.events = {};
                    if (!(uA = zO.handle))
                        uA = zO.handle = function(ZA) {
                            return typeof yU !== "undefined" && yU.event.triggered !== ZA.type ? yU.event.dispatch.apply(rx, arguments) : void 0
                        }
                        ;
                    ZA = (ZA || "").match(oD) || [""],
                    Us = ZA.length;
                    while (Us--) {
                        if (XH = Jc.exec(ZA[Us]) || [],
                        yi = Ze = XH[1],
                        ny = (XH[2] || "").split(".").sort(),
                        !yi)
                            continue;
                        if (VX = yU.event.special[yi] || {},
                        yi = (lo ? VX.delegateType : VX.bindType) || yi,
                        VX = yU.event.special[yi] || {},
                        ZQ = yU.extend({
                            type: yi,
                            origType: Ze,
                            data: wB,
                            handler: Tb,
                            guid: Tb.guid,
                            selector: lo,
                            needsContext: lo && yU.expr.match.needsContext.test(lo),
                            namespace: ny.join(".")
                        }, zC),
                        !(SO = OE[yi]))
                            if (SO = OE[yi] = [],
                            SO.delegateCount = 0,
                            !VX.setup || VX.setup.call(rx, wB, ny, uA) === false)
                                if (rx.addEventListener)
                                    rx.addEventListener(yi, uA);
                        if (VX.add)
                            if (VX.add.call(rx, ZQ),
                            !ZQ.handler.guid)
                                ZQ.handler.guid = Tb.guid;
                        if (lo)
                            SO.splice(SO.delegateCount++, 0, ZQ);
                        else
                            SO.push(ZQ);
                        yU.event.global[yi] = true
                    }
                },
                remove: function(rx, ZA, Tb, wB, lo) {
                    var zC, uA, XH, OE, Us, ZQ, VX, SO, yi, ny, Ze, zO = tv.hasData(rx) && tv.get(rx);
                    if (!zO || !(OE = zO.events))
                        return;
                    ZA = (ZA || "").match(oD) || [""],
                    Us = ZA.length;
                    while (Us--) {
                        if (XH = Jc.exec(ZA[Us]) || [],
                        yi = Ze = XH[1],
                        ny = (XH[2] || "").split(".").sort(),
                        !yi) {
                            for (yi in OE)
                                yU.event.remove(rx, yi + ZA[Us], Tb, wB, true);
                            continue
                        }
                        VX = yU.event.special[yi] || {},
                        yi = (wB ? VX.delegateType : VX.bindType) || yi,
                        SO = OE[yi] || [],
                        XH = XH[2] && new RegExp("(^|\\.)" + ny.join("\\.(?:.*\\.|)") + "(\\.|$)"),
                        uA = zC = SO.length;
                        while (zC--)
                            if (ZQ = SO[zC],
                            (lo || Ze === ZQ.origType) && (!Tb || Tb.guid === ZQ.guid) && (!XH || XH.test(ZQ.namespace)) && (!wB || wB === ZQ.selector || wB === "**" && ZQ.selector)) {
                                if (SO.splice(zC, 1),
                                ZQ.selector)
                                    SO.delegateCount--;
                                if (VX.remove)
                                    VX.remove.call(rx, ZQ)
                            }
                        if (uA && !SO.length) {
                            if (!VX.teardown || VX.teardown.call(rx, ny, zO.handle) === false)
                                yU.removeEvent(rx, yi, zO.handle);
                            delete OE[yi]
                        }
                    }
                    if (yU.isEmptyObject(OE))
                        tv.remove(rx, "handle events")
                },
                dispatch: function(rx) {
                    var ZA = yU.event.fix(rx), Tb, wB, lo, zC, uA, XH, OE = new Array(arguments.length), Us = (tv.get(this, "events") || {})[ZA.type] || [], ZQ = yU.event.special[ZA.type] || {};
                    for (OE[0] = ZA,
                    Tb = 1; Tb < arguments.length; Tb++)
                        OE[Tb] = arguments[Tb];
                    if (ZA.delegateTarget = this,
                    ZQ.preDispatch && ZQ.preDispatch.call(this, ZA) === false)
                        return;
                    XH = yU.event.handlers.call(this, ZA, Us),
                    Tb = 0;
                    while ((zC = XH[Tb++]) && !ZA.isPropagationStopped()) {
                        ZA.currentTarget = zC.elem,
                        wB = 0;
                        while ((uA = zC.handlers[wB++]) && !ZA.isImmediatePropagationStopped())
                            if (!ZA.rnamespace || uA.namespace === false || ZA.rnamespace.test(uA.namespace))
                                if (ZA.handleObj = uA,
                                ZA.data = uA.data,
                                lo = ((yU.event.special[uA.origType] || {}).handle || uA.handler).apply(zC.elem, OE),
                                lo !== void 0)
                                    if ((ZA.result = lo) === false)
                                        ZA.preventDefault(),
                                        ZA.stopPropagation()
                    }
                    if (ZQ.postDispatch)
                        ZQ.postDispatch.call(this, ZA);
                    return ZA.result
                },
                handlers: function(rx, ZA) {
                    var Tb, wB, lo, zC, uA, XH = [], OE = ZA.delegateCount, Us = rx.target;
                    if (OE && Us.nodeType && !(rx.type === "click" && rx.button >= 1))
                        for (; Us !== this; Us = Us.parentNode || this)
                            if (Us.nodeType === 1 && !(rx.type === "click" && Us.disabled === true)) {
                                for (zC = [],
                                uA = {},
                                Tb = 0; Tb < OE; Tb++) {
                                    if (wB = ZA[Tb],
                                    lo = wB.selector + " ",
                                    uA[lo] === void 0)
                                        uA[lo] = wB.needsContext ? yU(lo, this).index(Us) > -1 : yU.find(lo, this, null, [Us]).length;
                                    if (uA[lo])
                                        zC.push(wB)
                                }
                                if (zC.length)
                                    XH.push({
                                        elem: Us,
                                        handlers: zC
                                    })
                            }
                    if (Us = this,
                    OE < ZA.length)
                        XH.push({
                            elem: Us,
                            handlers: ZA.slice(OE)
                        });
                    return XH
                },
                addProp: function(rx, ZA) {
                    Object.defineProperty(yU.Event.prototype, rx, {
                        enumerable: true,
                        configurable: true,
                        get: Ze(ZA) ? function() {
                            if (this.originalEvent)
                                return ZA(this.originalEvent)
                        }
                        : function() {
                            if (this.originalEvent)
                                return this.originalEvent[rx]
                        }
                        ,
                        set: function(ZA) {
                            Object.defineProperty(this, rx, {
                                enumerable: true,
                                configurable: true,
                                writable: true,
                                value: ZA
                            })
                        }
                    })
                },
                fix: function(rx) {
                    return rx[yU.expando] ? rx : new yU.Event(rx)
                },
                special: {
                    load: {
                        noBubble: true
                    },
                    click: {
                        setup: function(rx) {
                            var ZA = this || rx;
                            if (Kd.test(ZA.type) && ZA.click && UI(ZA, "input"))
                                ry(ZA, "click", as);
                            return false
                        },
                        trigger: function(rx) {
                            var ZA = this || rx;
                            if (Kd.test(ZA.type) && ZA.click && UI(ZA, "input"))
                                ry(ZA, "click");
                            return true
                        },
                        _default: function(rx) {
                            var ZA = rx.target;
                            return Kd.test(ZA.type) && ZA.click && UI(ZA, "input") && tv.get(ZA, "click") || UI(ZA, "a")
                        }
                    },
                    beforeunload: {
                        postDispatch: function(rx) {
                            if (rx.result !== void 0 && rx.originalEvent)
                                rx.originalEvent.returnValue = rx.result
                        }
                    }
                }
            },
            yU.removeEvent = function(rx, ZA, Tb) {
                if (rx.removeEventListener)
                    rx.removeEventListener(ZA, Tb)
            }
            ,
            yU.Event = function(rx, ZA) {
                if (!(this instanceof yU.Event))
                    return new yU.Event(rx,ZA);
                if (rx && rx.type)
                    this.originalEvent = rx,
                    this.type = rx.type,
                    this.isDefaultPrevented = rx.defaultPrevented || rx.defaultPrevented === void 0 && rx.returnValue === false ? as : qj,
                    this.target = rx.target && rx.target.nodeType === 3 ? rx.target.parentNode : rx.target,
                    this.currentTarget = rx.currentTarget,
                    this.relatedTarget = rx.relatedTarget;
                else
                    this.type = rx;
                if (ZA)
                    yU.extend(this, ZA);
                this.timeStamp = rx && rx.timeStamp || Date.now(),
                this[yU.expando] = true
            }
            ,
            yU.Event.prototype = {
                constructor: yU.Event,
                isDefaultPrevented: qj,
                isPropagationStopped: qj,
                isImmediatePropagationStopped: qj,
                isSimulated: false,
                preventDefault: function() {
                    var rx = this.originalEvent;
                    if (this.isDefaultPrevented = as,
                    rx && !this.isSimulated)
                        rx.preventDefault()
                },
                stopPropagation: function() {
                    var rx = this.originalEvent;
                    if (this.isPropagationStopped = as,
                    rx && !this.isSimulated)
                        rx.stopPropagation()
                },
                stopImmediatePropagation: function() {
                    var rx = this.originalEvent;
                    if (this.isImmediatePropagationStopped = as,
                    rx && !this.isSimulated)
                        rx.stopImmediatePropagation();
                    this.stopPropagation()
                }
            },
            yU.each({
                altKey: true,
                bubbles: true,
                cancelable: true,
                changedTouches: true,
                ctrlKey: true,
                detail: true,
                eventPhase: true,
                metaKey: true,
                pageX: true,
                pageY: true,
                shiftKey: true,
                view: true,
                char: true,
                code: true,
                charCode: true,
                key: true,
                keyCode: true,
                button: true,
                buttons: true,
                clientX: true,
                clientY: true,
                offsetX: true,
                offsetY: true,
                pointerId: true,
                pointerType: true,
                screenX: true,
                screenY: true,
                targetTouches: true,
                toElement: true,
                touches: true,
                which: function(rx) {
                    var ZA = rx.button;
                    if (rx.which == null && Lq.test(rx.type))
                        return rx.charCode != null ? rx.charCode : rx.keyCode;
                    if (!rx.which && ZA !== void 0 && Ag.test(rx.type)) {
                        if (ZA & 1)
                            return 1;
                        if (ZA & 2)
                            return 3;
                        if (ZA & 4)
                            return 2;
                        return 0
                    }
                    return rx.which
                }
            }, yU.event.addProp),
            yU.each({
                focus: "focusin",
                blur: "focusout"
            }, (function(rx, ZA) {
                yU.event.special[rx] = {
                    setup: function() {
                        return ry(this, rx, pA),
                        false
                    },
                    trigger: function() {
                        return ry(this, rx),
                        true
                    },
                    delegateType: ZA
                }
            }
            )),
            yU.each({
                mouseenter: "mouseover",
                mouseleave: "mouseout",
                pointerenter: "pointerover",
                pointerleave: "pointerout"
            }, (function(rx, ZA) {
                yU.event.special[rx] = {
                    delegateType: ZA,
                    bindType: ZA,
                    handle: function(rx) {
                        var Tb, wB = this, lo = rx.relatedTarget, zC = rx.handleObj;
                        if (!lo || lo !== wB && !yU.contains(wB, lo))
                            rx.type = zC.origType,
                            Tb = zC.handler.apply(this, arguments),
                            rx.type = ZA;
                        return Tb
                    }
                }
            }
            )),
            yU.fn.extend({
                on: function(rx, ZA, Tb, wB) {
                    return Fa(this, rx, ZA, Tb, wB)
                },
                one: function(rx, ZA, Tb, wB) {
                    return Fa(this, rx, ZA, Tb, wB, 1)
                },
                off: function(rx, ZA, Tb) {
                    var wB, lo;
                    if (rx && rx.preventDefault && rx.handleObj)
                        return wB = rx.handleObj,
                        yU(rx.delegateTarget).off(wB.namespace ? wB.origType + "." + wB.namespace : wB.origType, wB.selector, wB.handler),
                        this;
                    if (typeof rx === "object") {
                        for (lo in rx)
                            this.off(lo, ZA, rx[lo]);
                        return this
                    }
                    if (ZA === false || typeof ZA === "function")
                        Tb = ZA,
                        ZA = void 0;
                    if (Tb === false)
                        Tb = qj;
                    return this.each((function() {
                        yU.event.remove(this, rx, Tb, ZA)
                    }
                    ))
                }
            });
            var xi = /<(?!area|br|col|embed|hr|img|input|link|meta|param)(([a-z][^\/\0>\x20\t\r\n\f]*)[^>]*)\/>/gi
              , Ro = /<script|<style|<link/i
              , Hv = /checked\s*(?:[^=]|=\s*.checked.)/i
              , Ia = /^\s*<!(?:\[CDATA\[|--)|(?:\]\]|--)>\s*$/g;
            function Tu(rx, ZA) {
                if (UI(rx, "table") && UI(ZA.nodeType !== 11 ? ZA : ZA.firstChild, "tr"))
                    return yU(rx).children("tbody")[0] || rx;
                return rx
            }
            function zy(rx) {
                return rx.type = (rx.getAttribute("type") !== null) + "/" + rx.type,
                rx
            }
            function Vp(rx) {
                if ((rx.type || "").slice(0, 5) === "true/")
                    rx.type = rx.type.slice(5);
                else
                    rx.removeAttribute("type");
                return rx
            }
            function Zs(rx, ZA) {
                var Tb, wB, lo, zC, uA, XH, OE, Us;
                if (ZA.nodeType !== 1)
                    return;
                if (tv.hasData(rx))
                    if (zC = tv.access(rx),
                    uA = tv.set(ZA, zC),
                    Us = zC.events,
                    Us)
                        for (lo in delete uA.handle,
                        uA.events = {},
                        Us)
                            for (Tb = 0,
                            wB = Us[lo].length; Tb < wB; Tb++)
                                yU.event.add(ZA, lo, Us[lo][Tb]);
                if (Xo.hasData(rx))
                    XH = Xo.access(rx),
                    OE = yU.extend({}, XH),
                    Xo.set(ZA, OE)
            }
            function Tj(rx, ZA) {
                var Tb = ZA.nodeName.toLowerCase();
                if (Tb === "input" && Kd.test(rx.type))
                    ZA.checked = rx.checked;
                else if (Tb === "input" || Tb === "textarea")
                    ZA.defaultValue = rx.defaultValue
            }
            function nt(rx, ZA, Tb, wB) {
                ZA = uA.apply([], ZA);
                var lo, zC, XH, OE, Us, ZQ, VX = 0, SO = rx.length, yi = SO - 1, zO = ZA[0], Fc = Ze(zO);
                if (Fc || SO > 1 && typeof zO === "string" && !ny.checkClone && Hv.test(zO))
                    return rx.each((function(lo) {
                        var zC = rx.eq(lo);
                        if (Fc)
                            ZA[0] = zO.call(this, lo, zC.html());
                        nt(zC, ZA, Tb, wB)
                    }
                    ));
                if (SO) {
                    if (lo = NX(ZA, rx[0].ownerDocument, false, rx, wB),
                    zC = lo.firstChild,
                    lo.childNodes.length === 1)
                        lo = zC;
                    if (zC || wB) {
                        for (XH = yU.map(ON(lo, "script"), zy),
                        OE = XH.length; VX < SO; VX++) {
                            if (Us = lo,
                            VX !== yi)
                                if (Us = yU.clone(Us, true, true),
                                OE)
                                    yU.merge(XH, ON(Us, "script"));
                            Tb.call(rx[VX], Us, VX)
                        }
                        if (OE)
                            for (ZQ = XH[XH.length - 1].ownerDocument,
                            yU.map(XH, Vp),
                            VX = 0; VX < OE; VX++)
                                if (Us = XH[VX],
                                Fy.test(Us.type || "") && !tv.access(Us, "globalEval") && yU.contains(ZQ, Us))
                                    if (Us.src && (Us.type || "").toLowerCase() !== "module") {
                                        if (yU._evalUrl && !Us.noModule)
                                            yU._evalUrl(Us.src, {
                                                nonce: Us.nonce || Us.getAttribute("nonce")
                                            })
                                    } else
                                        yI(Us.textContent.replace(Ia, ""), Us, ZQ)
                    }
                }
                return rx
            }
            function DQ(rx, ZA, Tb) {
                for (var wB, lo = ZA ? yU.filter(ZA, rx) : rx, zC = 0; (wB = lo[zC]) != null; zC++) {
                    if (!Tb && wB.nodeType === 1)
                        yU.cleanData(ON(wB));
                    if (wB.parentNode) {
                        if (Tb && hQ(wB))
                            cU(ON(wB, "script"));
                        wB.parentNode.removeChild(wB)
                    }
                }
                return rx
            }
            yU.extend({
                htmlPrefilter: function(rx) {
                    return rx.replace(xi, "<$1></$2>")
                },
                clone: function(rx, ZA, Tb) {
                    var wB, lo, zC, uA, XH = rx.cloneNode(true), OE = hQ(rx);
                    if (!ny.noCloneChecked && (rx.nodeType === 1 || rx.nodeType === 11) && !yU.isXMLDoc(rx))
                        for (uA = ON(XH),
                        zC = ON(rx),
                        wB = 0,
                        lo = zC.length; wB < lo; wB++)
                            Tj(zC[wB], uA[wB]);
                    if (ZA)
                        if (Tb)
                            for (zC = zC || ON(rx),
                            uA = uA || ON(XH),
                            wB = 0,
                            lo = zC.length; wB < lo; wB++)
                                Zs(zC[wB], uA[wB]);
                        else
                            Zs(rx, XH);
                    if (uA = ON(XH, "script"),
                    uA.length > 0)
                        cU(uA, !OE && ON(rx, "script"));
                    return XH
                },
                cleanData: function(rx) {
                    for (var ZA, Tb, wB, lo = yU.event.special, zC = 0; (Tb = rx[zC]) !== void 0; zC++)
                        if (uH(Tb)) {
                            if (ZA = Tb[tv.expando]) {
                                if (ZA.events)
                                    for (wB in ZA.events)
                                        if (lo[wB])
                                            yU.event.remove(Tb, wB);
                                        else
                                            yU.removeEvent(Tb, wB, ZA.handle);
                                Tb[tv.expando] = void 0
                            }
                            if (Tb[Xo.expando])
                                Tb[Xo.expando] = void 0
                        }
                }
            }),
            yU.fn.extend({
                detach: function(rx) {
                    return DQ(this, rx, true)
                },
                remove: function(rx) {
                    return DQ(this, rx)
                },
                text: function(rx) {
                    return wJ(this, (function(rx) {
                        return rx === void 0 ? yU.text(this) : this.empty().each((function() {
                            if (this.nodeType === 1 || this.nodeType === 11 || this.nodeType === 9)
                                this.textContent = rx
                        }
                        ))
                    }
                    ), null, rx, arguments.length)
                },
                append: function() {
                    return nt(this, arguments, (function(rx) {
                        if (this.nodeType === 1 || this.nodeType === 11 || this.nodeType === 9) {
                            var ZA = Tu(this, rx);
                            ZA.appendChild(rx)
                        }
                    }
                    ))
                },
                prepend: function() {
                    return nt(this, arguments, (function(rx) {
                        if (this.nodeType === 1 || this.nodeType === 11 || this.nodeType === 9) {
                            var ZA = Tu(this, rx);
                            ZA.insertBefore(rx, ZA.firstChild)
                        }
                    }
                    ))
                },
                before: function() {
                    return nt(this, arguments, (function(rx) {
                        if (this.parentNode)
                            this.parentNode.insertBefore(rx, this)
                    }
                    ))
                },
                after: function() {
                    return nt(this, arguments, (function(rx) {
                        if (this.parentNode)
                            this.parentNode.insertBefore(rx, this.nextSibling)
                    }
                    ))
                },
                empty: function() {
                    for (var rx, ZA = 0; (rx = this[ZA]) != null; ZA++)
                        if (rx.nodeType === 1)
                            yU.cleanData(ON(rx, false)),
                            rx.textContent = "";
                    return this
                },
                clone: function(rx, ZA) {
                    return rx = rx == null ? false : rx,
                    ZA = ZA == null ? rx : ZA,
                    this.map((function() {
                        return yU.clone(this, rx, ZA)
                    }
                    ))
                },
                html: function(rx) {
                    return wJ(this, (function(rx) {
                        var ZA = this[0] || {}
                          , Tb = 0
                          , wB = this.length;
                        if (rx === void 0 && ZA.nodeType === 1)
                            return ZA.innerHTML;
                        if (typeof rx === "string" && !Ro.test(rx) && !Lb[(Zb.exec(rx) || ["", ""])[1].toLowerCase()]) {
                            rx = yU.htmlPrefilter(rx);
                            try {
                                for (; Tb < wB; Tb++)
                                    if (ZA = this[Tb] || {},
                                    ZA.nodeType === 1)
                                        yU.cleanData(ON(ZA, false)),
                                        ZA.innerHTML = rx;
                                ZA = 0
                            } catch (rx) {}
                        }
                        if (ZA)
                            this.empty().append(rx)
                    }
                    ), null, rx, arguments.length)
                },
                replaceWith: function() {
                    var rx = [];
                    return nt(this, arguments, (function(ZA) {
                        var Tb = this.parentNode;
                        if (yU.inArray(this, rx) < 0)
                            if (yU.cleanData(ON(this)),
                            Tb)
                                Tb.replaceChild(ZA, this)
                    }
                    ), rx)
                }
            }),
            yU.each({
                appendTo: "append",
                prependTo: "prepend",
                insertBefore: "before",
                insertAfter: "after",
                replaceAll: "replaceWith"
            }, (function(rx, ZA) {
                yU.fn[rx] = function(rx) {
                    for (var Tb, wB = [], lo = yU(rx), zC = lo.length - 1, uA = 0; uA <= zC; uA++)
                        Tb = uA === zC ? this : this.clone(true),
                        yU(lo[uA])[ZA](Tb),
                        XH.apply(wB, Tb.get());
                    return this.pushStack(wB)
                }
            }
            ));
            var CR = new RegExp("^(" + fL + ")(?!px)[a-z%]+$","i")
              , el = function(ZA) {
                var Tb = ZA.ownerDocument.defaultView;
                if (!Tb || !Tb.opener)
                    Tb = rx;
                return Tb.getComputedStyle(ZA)
            }
              , Ii = new RegExp(GW.join("|"),"i");
            function FZ(rx, ZA, Tb) {
                var wB, lo, zC, uA, XH = rx.style;
                if (Tb = Tb || el(rx),
                Tb) {
                    if (uA = Tb.getPropertyValue(ZA) || Tb[ZA],
                    uA === "" && !hQ(rx))
                        uA = yU.style(rx, ZA);
                    if (!ny.pixelBoxStyles() && CR.test(uA) && Ii.test(ZA))
                        wB = XH.width,
                        lo = XH.minWidth,
                        zC = XH.maxWidth,
                        XH.minWidth = XH.maxWidth = XH.width = uA,
                        uA = Tb.width,
                        XH.width = wB,
                        XH.minWidth = lo,
                        XH.maxWidth = zC
                }
                return uA !== void 0 ? uA + "" : uA
            }
            function Qm(rx, ZA) {
                return {
                    get: function() {
                        if (rx())
                            return void delete this.get;
                        return (this.get = ZA).apply(this, arguments)
                    }
                }
            }
            (function() {
                function ZA() {
                    if (!ZQ)
                        return;
                    Us.style.cssText = "position:absolute;left:-11111px;width:60px;" + "margin-top:1px;padding:0;border:0",
                    ZQ.style.cssText = "position:relative;display:block;box-sizing:border-box;overflow:scroll;" + "margin:auto;border:1px;padding:1px;" + "width:60%;top:1%",
                    nx.appendChild(Us).appendChild(ZQ);
                    var ZA = rx.getComputedStyle(ZQ);
                    lo = ZA.top !== "1%",
                    OE = Tb(ZA.marginLeft) === 12,
                    ZQ.style.right = "60%",
                    XH = Tb(ZA.right) === 36,
                    zC = Tb(ZA.width) === 36,
                    ZQ.style.position = "absolute",
                    uA = Tb(ZQ.offsetWidth / 3) === 12,
                    nx.removeChild(Us),
                    ZQ = null
                }
                function Tb(rx) {
                    return Math.round(parseFloat(rx))
                }
                var lo, zC, uA, XH, OE, Us = wB.createElement("div"), ZQ = wB.createElement("div");
                if (!ZQ.style)
                    return;
                ZQ.style.backgroundClip = "content-box",
                ZQ.cloneNode(true).style.backgroundClip = "",
                ny.clearCloneStyle = ZQ.style.backgroundClip === "content-box",
                yU.extend(ny, {
                    boxSizingReliable: function() {
                        return ZA(),
                        zC
                    },
                    pixelBoxStyles: function() {
                        return ZA(),
                        XH
                    },
                    pixelPosition: function() {
                        return ZA(),
                        lo
                    },
                    reliableMarginLeft: function() {
                        return ZA(),
                        OE
                    },
                    scrollboxSize: function() {
                        return ZA(),
                        uA
                    }
                })
            }
            )();
            var Qj = ["Webkit", "Moz", "ms"]
              , YK = wB.createElement("div").style
              , TN = {};
            function qa(rx) {
                var ZA = rx[0].toUpperCase() + rx.slice(1)
                  , Tb = Qj.length;
                while (Tb--)
                    if (rx = Qj[Tb] + ZA,
                    rx in YK)
                        return rx
            }
            function ub(rx) {
                var ZA = yU.cssProps[rx] || TN[rx];
                if (ZA)
                    return ZA;
                if (rx in YK)
                    return rx;
                return TN[rx] = qa(rx) || rx
            }
            var wW = /^(none|table(?!-c[ea]).+)/
              , uZ = /^--/
              , zZ = {
                position: "absolute",
                visibility: "hidden",
                display: "block"
            }
              , NK = {
                letterSpacing: "0",
                fontWeight: "400"
            };
            function pL(rx, ZA, Tb) {
                var wB = ar.exec(ZA);
                return wB ? Math.max(0, wB[2] - (Tb || 0)) + (wB[3] || "px") : ZA
            }
            function FI(rx, ZA, Tb, wB, lo, zC) {
                var uA = ZA === "width" ? 1 : 0
                  , XH = 0
                  , OE = 0;
                if (Tb === (wB ? "border" : "content"))
                    return 0;
                for (; uA < 4; uA += 2) {
                    if (Tb === "margin")
                        OE += yU.css(rx, Tb + GW[uA], true, lo);
                    if (!wB)
                        if (OE += yU.css(rx, "padding" + GW[uA], true, lo),
                        Tb !== "padding")
                            OE += yU.css(rx, "border" + GW[uA] + "Width", true, lo);
                        else
                            XH += yU.css(rx, "border" + GW[uA] + "Width", true, lo);
                    else {
                        if (Tb === "content")
                            OE -= yU.css(rx, "padding" + GW[uA], true, lo);
                        if (Tb !== "margin")
                            OE -= yU.css(rx, "border" + GW[uA] + "Width", true, lo)
                    }
                }
                if (!wB && zC >= 0)
                    OE += Math.max(0, Math.ceil(rx["offset" + ZA[0].toUpperCase() + ZA.slice(1)] - zC - OE - XH - .5)) || 0;
                return OE
            }
            function kW(rx, ZA, Tb) {
                var wB = el(rx)
                  , lo = !ny.boxSizingReliable() || Tb
                  , zC = lo && yU.css(rx, "boxSizing", false, wB) === "border-box"
                  , uA = zC
                  , XH = FZ(rx, ZA, wB)
                  , OE = "offset" + ZA[0].toUpperCase() + ZA.slice(1);
                if (CR.test(XH)) {
                    if (!Tb)
                        return XH;
                    XH = "auto"
                }
                if ((!ny.boxSizingReliable() && zC || XH === "auto" || !parseFloat(XH) && yU.css(rx, "display", false, wB) === "inline") && rx.getClientRects().length)
                    if (zC = yU.css(rx, "boxSizing", false, wB) === "border-box",
                    uA = OE in rx,
                    uA)
                        XH = rx[OE];
                return XH = parseFloat(XH) || 0,
                XH + FI(rx, ZA, Tb || (zC ? "border" : "content"), uA, wB, XH) + "px"
            }
            function wI(rx, ZA, Tb, wB, lo) {
                return new wI.prototype.init(rx,ZA,Tb,wB,lo)
            }
            yU.extend({
                cssHooks: {
                    opacity: {
                        get: function(rx, ZA) {
                            if (ZA) {
                                var Tb = FZ(rx, "opacity");
                                return Tb === "" ? "1" : Tb
                            }
                        }
                    }
                },
                cssNumber: {
                    animationIterationCount: true,
                    columnCount: true,
                    fillOpacity: true,
                    flexGrow: true,
                    flexShrink: true,
                    fontWeight: true,
                    gridArea: true,
                    gridColumn: true,
                    gridColumnEnd: true,
                    gridColumnStart: true,
                    gridRow: true,
                    gridRowEnd: true,
                    gridRowStart: true,
                    lineHeight: true,
                    opacity: true,
                    order: true,
                    orphans: true,
                    widows: true,
                    zIndex: true,
                    zoom: true
                },
                cssProps: {},
                style: function(rx, ZA, Tb, wB) {
                    if (!rx || rx.nodeType === 3 || rx.nodeType === 8 || !rx.style)
                        return;
                    var lo, zC, uA, XH = xh(ZA), OE = uZ.test(ZA), Us = rx.style;
                    if (!OE)
                        ZA = ub(XH);
                    if (uA = yU.cssHooks[ZA] || yU.cssHooks[XH],
                    Tb !== void 0) {
                        if (zC = typeof Tb,
                        zC === "string" && (lo = ar.exec(Tb)) && lo[1])
                            Tb = yJ(rx, ZA, lo),
                            zC = "number";
                        if (Tb == null || Tb !== Tb)
                            return;
                        if (zC === "number" && !OE)
                            Tb += lo && lo[3] || (yU.cssNumber[XH] ? "" : "px");
                        if (!ny.clearCloneStyle && Tb === "" && ZA.indexOf("background") === 0)
                            Us[ZA] = "inherit";
                        if (!uA || !("set" in uA) || (Tb = uA.set(rx, Tb, wB)) !== void 0)
                            if (OE)
                                Us.setProperty(ZA, Tb);
                            else
                                Us[ZA] = Tb
                    } else {
                        if (uA && "get" in uA && (lo = uA.get(rx, false, wB)) !== void 0)
                            return lo;
                        return Us[ZA]
                    }
                },
                css: function(rx, ZA, Tb, wB) {
                    var lo, zC, uA, XH = xh(ZA), OE = uZ.test(ZA);
                    if (!OE)
                        ZA = ub(XH);
                    if (uA = yU.cssHooks[ZA] || yU.cssHooks[XH],
                    uA && "get" in uA)
                        lo = uA.get(rx, true, Tb);
                    if (lo === void 0)
                        lo = FZ(rx, ZA, wB);
                    if (lo === "normal" && ZA in NK)
                        lo = NK[ZA];
                    if (Tb === "" || Tb)
                        return zC = parseFloat(lo),
                        Tb === true || isFinite(zC) ? zC || 0 : lo;
                    return lo
                }
            }),
            yU.each(["height", "width"], (function(rx, ZA) {
                yU.cssHooks[ZA] = {
                    get: function(rx, Tb, wB) {
                        if (Tb)
                            return wW.test(yU.css(rx, "display")) && (!rx.getClientRects().length || !rx.getBoundingClientRect().width) ? et(rx, zZ, (function() {
                                return kW(rx, ZA, wB)
                            }
                            )) : kW(rx, ZA, wB)
                    },
                    set: function(rx, Tb, wB) {
                        var lo, zC = el(rx), uA = !ny.scrollboxSize() && zC.position === "absolute", XH = uA || wB, OE = XH && yU.css(rx, "boxSizing", false, zC) === "border-box", Us = wB ? FI(rx, ZA, wB, OE, zC) : 0;
                        if (OE && uA)
                            Us -= Math.ceil(rx["offset" + ZA[0].toUpperCase() + ZA.slice(1)] - parseFloat(zC[ZA]) - FI(rx, ZA, "border", false, zC) - .5);
                        if (Us && (lo = ar.exec(Tb)) && (lo[3] || "px") !== "px")
                            rx.style[ZA] = Tb,
                            Tb = yU.css(rx, ZA);
                        return pL(rx, Tb, Us)
                    }
                }
            }
            )),
            yU.cssHooks.marginLeft = Qm(ny.reliableMarginLeft, (function(rx, ZA) {
                if (ZA)
                    return (parseFloat(FZ(rx, "marginLeft")) || rx.getBoundingClientRect().left - et(rx, {
                        marginLeft: 0
                    }, (function() {
                        return rx.getBoundingClientRect().left
                    }
                    ))) + "px"
            }
            )),
            yU.each({
                margin: "",
                padding: "",
                border: "Width"
            }, (function(rx, ZA) {
                if (yU.cssHooks[rx + ZA] = {
                    expand: function(Tb) {
                        for (var wB = 0, lo = {}, zC = typeof Tb === "string" ? Tb.split(" ") : [Tb]; wB < 4; wB++)
                            lo[rx + GW[wB] + ZA] = zC[wB] || zC[wB - 2] || zC[0];
                        return lo
                    }
                },
                rx !== "margin")
                    yU.cssHooks[rx + ZA].set = pL
            }
            )),
            yU.fn.extend({
                css: function(rx, ZA) {
                    return wJ(this, (function(rx, ZA, Tb) {
                        var wB, lo, zC = {}, uA = 0;
                        if (Array.isArray(ZA)) {
                            for (wB = el(rx),
                            lo = ZA.length; uA < lo; uA++)
                                zC[ZA[uA]] = yU.css(rx, ZA[uA], false, wB);
                            return zC
                        }
                        return Tb !== void 0 ? yU.style(rx, ZA, Tb) : yU.css(rx, ZA)
                    }
                    ), rx, ZA, arguments.length > 1)
                }
            }),
            yU.Tween = wI,
            wI.prototype = {
                constructor: wI,
                init: function(rx, ZA, Tb, wB, lo, zC) {
                    this.elem = rx,
                    this.prop = Tb,
                    this.easing = lo || yU.easing._default,
                    this.options = ZA,
                    this.start = this.now = this.cur(),
                    this.end = wB,
                    this.unit = zC || (yU.cssNumber[Tb] ? "" : "px")
                },
                cur: function() {
                    var rx = wI.propHooks[this.prop];
                    return rx && rx.get ? rx.get(this) : wI.propHooks._default.get(this)
                },
                run: function(rx) {
                    var ZA, Tb = wI.propHooks[this.prop];
                    if (this.options.duration)
                        this.pos = ZA = yU.easing[this.easing](rx, this.options.duration * rx, 0, 1, this.options.duration);
                    else
                        this.pos = ZA = rx;
                    if (this.now = (this.end - this.start) * ZA + this.start,
                    this.options.step)
                        this.options.step.call(this.elem, this.now, this);
                    if (Tb && Tb.set)
                        Tb.set(this);
                    else
                        wI.propHooks._default.set(this);
                    return this
                }
            },
            wI.prototype.init.prototype = wI.prototype,
            wI.propHooks = {
                _default: {
                    get: function(rx) {
                        var ZA;
                        if (rx.elem.nodeType !== 1 || rx.elem[rx.prop] != null && rx.elem.style[rx.prop] == null)
                            return rx.elem[rx.prop];
                        return ZA = yU.css(rx.elem, rx.prop, ""),
                        !ZA || ZA === "auto" ? 0 : ZA
                    },
                    set: function(rx) {
                        if (yU.fx.step[rx.prop])
                            yU.fx.step[rx.prop](rx);
                        else if (rx.elem.nodeType === 1 && (yU.cssHooks[rx.prop] || rx.elem.style[ub(rx.prop)] != null))
                            yU.style(rx.elem, rx.prop, rx.now + rx.unit);
                        else
                            rx.elem[rx.prop] = rx.now
                    }
                }
            },
            wI.propHooks.scrollTop = wI.propHooks.scrollLeft = {
                set: function(rx) {
                    if (rx.elem.nodeType && rx.elem.parentNode)
                        rx.elem[rx.prop] = rx.now
                }
            },
            yU.easing = {
                linear: function(rx) {
                    return rx
                },
                swing: function(rx) {
                    return .5 - Math.cos(rx * Math.PI) / 2
                },
                _default: "swing"
            },
            yU.fx = wI.prototype.init,
            yU.fx.step = {};
            var bt, hX, pf = /^(?:toggle|show|hide)$/, Zj = /queueHooks$/;
            function oH() {
                if (hX) {
                    if (wB.hidden === false && rx.requestAnimationFrame)
                        rx.requestAnimationFrame(oH);
                    else
                        rx.setTimeout(oH, yU.fx.interval);
                    yU.fx.tick()
                }
            }
            function AP() {
                return rx.setTimeout((function() {
                    bt = void 0
                }
                )),
                bt = Date.now()
            }
            function sJ(rx, ZA) {
                var Tb, wB = 0, lo = {
                    height: rx
                };
                for (ZA = ZA ? 1 : 0; wB < 4; wB += 2 - ZA)
                    Tb = GW[wB],
                    lo["margin" + Tb] = lo["padding" + Tb] = rx;
                if (ZA)
                    lo.opacity = lo.width = rx;
                return lo
            }
            function wq(rx, ZA, Tb) {
                for (var wB, lo = (we.tweeners[ZA] || []).concat(we.tweeners["*"]), zC = 0, uA = lo.length; zC < uA; zC++)
                    if (wB = lo[zC].call(Tb, ZA, rx))
                        return wB
            }
            function bC(rx, ZA, Tb) {
                var wB, lo, zC, uA, XH, OE, Us, ZQ, VX = "width" in ZA || "height" in ZA, SO = this, yi = {}, ny = rx.style, Ze = rx.nodeType && bs(rx), zO = tv.get(rx, "fxshow");
                if (!Tb.queue) {
                    if (uA = yU._queueHooks(rx, "fx"),
                    uA.unqueued == null)
                        uA.unqueued = 0,
                        XH = uA.empty.fire,
                        uA.empty.fire = function() {
                            if (!uA.unqueued)
                                XH()
                        }
                        ;
                    uA.unqueued++,
                    SO.always((function() {
                        SO.always((function() {
                            if (uA.unqueued--,
                            !yU.queue(rx, "fx").length)
                                uA.empty.fire()
                        }
                        ))
                    }
                    ))
                }
                for (wB in ZA)
                    if (lo = ZA[wB],
                    pf.test(lo)) {
                        if (delete ZA[wB],
                        zC = zC || lo === "toggle",
                        lo === (Ze ? "hide" : "show"))
                            if (lo === "show" && zO && zO[wB] !== void 0)
                                Ze = true;
                            else
                                continue;
                        yi[wB] = zO && zO[wB] || yU.style(rx, wB)
                    }
                if (OE = !yU.isEmptyObject(ZA),
                !OE && yU.isEmptyObject(yi))
                    return;
                if (VX && rx.nodeType === 1) {
                    if (Tb.overflow = [ny.overflow, ny.overflowX, ny.overflowY],
                    Us = zO && zO.display,
                    Us == null)
                        Us = tv.get(rx, "display");
                    if (ZQ = yU.css(rx, "display"),
                    ZQ === "none")
                        if (Us)
                            ZQ = Us;
                        else
                            Jt([rx], true),
                            Us = rx.style.display || Us,
                            ZQ = yU.css(rx, "display"),
                            Jt([rx]);
                    if (ZQ === "inline" || ZQ === "inline-block" && Us != null)
                        if (yU.css(rx, "float") === "none") {
                            if (!OE)
                                if (SO.done((function() {
                                    ny.display = Us
                                }
                                )),
                                Us == null)
                                    ZQ = ny.display,
                                    Us = ZQ === "none" ? "" : ZQ;
                            ny.display = "inline-block"
                        }
                }
                if (Tb.overflow)
                    ny.overflow = "hidden",
                    SO.always((function() {
                        ny.overflow = Tb.overflow[0],
                        ny.overflowX = Tb.overflow[1],
                        ny.overflowY = Tb.overflow[2]
                    }
                    ));
                for (wB in OE = false,
                yi) {
                    if (!OE) {
                        if (zO) {
                            if ("hidden" in zO)
                                Ze = zO.hidden
                        } else
                            zO = tv.access(rx, "fxshow", {
                                display: Us
                            });
                        if (zC)
                            zO.hidden = !Ze;
                        if (Ze)
                            Jt([rx], true);
                        SO.done((function() {
                            if (!Ze)
                                Jt([rx]);
                            for (wB in tv.remove(rx, "fxshow"),
                            yi)
                                yU.style(rx, wB, yi[wB])
                        }
                        ))
                    }
                    if (OE = wq(Ze ? zO[wB] : 0, wB, SO),
                    !(wB in zO))
                        if (zO[wB] = OE.start,
                        Ze)
                            OE.end = OE.start,
                            OE.start = 0
                }
            }
            function lz(rx, ZA) {
                var Tb, wB, lo, zC, uA;
                for (Tb in rx) {
                    if (wB = xh(Tb),
                    lo = ZA[wB],
                    zC = rx[Tb],
                    Array.isArray(zC))
                        lo = zC[1],
                        zC = rx[Tb] = zC[0];
                    if (Tb !== wB)
                        rx[wB] = zC,
                        delete rx[Tb];
                    if (uA = yU.cssHooks[wB],
                    uA && "expand" in uA) {
                        for (Tb in zC = uA.expand(zC),
                        delete rx[wB],
                        zC)
                            if (!(Tb in rx))
                                rx[Tb] = zC[Tb],
                                ZA[Tb] = lo
                    } else
                        ZA[wB] = lo
                }
            }
            function we(rx, ZA, Tb) {
                var wB, lo, zC = 0, uA = we.prefilters.length, XH = yU.Deferred().always((function() {
                    delete OE.elem
                }
                )), OE = function() {
                    if (lo)
                        return false;
                    for (var ZA = bt || AP(), Tb = Math.max(0, Us.startTime + Us.duration - ZA), wB = Tb / Us.duration || 0, zC = 1 - wB, uA = 0, OE = Us.tweens.length; uA < OE; uA++)
                        Us.tweens[uA].run(zC);
                    if (XH.notifyWith(rx, [Us, zC, Tb]),
                    zC < 1 && OE)
                        return Tb;
                    if (!OE)
                        XH.notifyWith(rx, [Us, 1, 0]);
                    return XH.resolveWith(rx, [Us]),
                    false
                }, Us = XH.promise({
                    elem: rx,
                    props: yU.extend({}, ZA),
                    opts: yU.extend(true, {
                        specialEasing: {},
                        easing: yU.easing._default
                    }, Tb),
                    originalProperties: ZA,
                    originalOptions: Tb,
                    startTime: bt || AP(),
                    duration: Tb.duration,
                    tweens: [],
                    createTween: function(ZA, Tb) {
                        var wB = yU.Tween(rx, Us.opts, ZA, Tb, Us.opts.specialEasing[ZA] || Us.opts.easing);
                        return Us.tweens.push(wB),
                        wB
                    },
                    stop: function(ZA) {
                        var Tb = 0
                          , wB = ZA ? Us.tweens.length : 0;
                        if (lo)
                            return this;
                        for (lo = true; Tb < wB; Tb++)
                            Us.tweens[Tb].run(1);
                        if (ZA)
                            XH.notifyWith(rx, [Us, 1, 0]),
                            XH.resolveWith(rx, [Us, ZA]);
                        else
                            XH.rejectWith(rx, [Us, ZA]);
                        return this
                    }
                }), ZQ = Us.props;
                for (lz(ZQ, Us.opts.specialEasing); zC < uA; zC++)
                    if (wB = we.prefilters[zC].call(Us, rx, ZQ, Us.opts),
                    wB) {
                        if (Ze(wB.stop))
                            yU._queueHooks(Us.elem, Us.opts.queue).stop = wB.stop.bind(wB);
                        return wB
                    }
                if (yU.map(ZQ, wq, Us),
                Ze(Us.opts.start))
                    Us.opts.start.call(rx, Us);
                return Us.progress(Us.opts.progress).done(Us.opts.done, Us.opts.complete).fail(Us.opts.fail).always(Us.opts.always),
                yU.fx.timer(yU.extend(OE, {
                    elem: rx,
                    anim: Us,
                    queue: Us.opts.queue
                })),
                Us
            }
            yU.Animation = yU.extend(we, {
                tweeners: {
                    "*": [function(rx, ZA) {
                        var Tb = this.createTween(rx, ZA);
                        return yJ(Tb.elem, rx, ar.exec(ZA), Tb),
                        Tb
                    }
                    ]
                },
                tweener: function(rx, ZA) {
                    if (Ze(rx))
                        ZA = rx,
                        rx = ["*"];
                    else
                        rx = rx.match(oD);
                    for (var Tb, wB = 0, lo = rx.length; wB < lo; wB++)
                        Tb = rx[wB],
                        we.tweeners[Tb] = we.tweeners[Tb] || [],
                        we.tweeners[Tb].unshift(ZA)
                },
                prefilters: [bC],
                prefilter: function(rx, ZA) {
                    if (ZA)
                        we.prefilters.unshift(rx);
                    else
                        we.prefilters.push(rx)
                }
            }),
            yU.speed = function(rx, ZA, Tb) {
                var wB = rx && typeof rx === "object" ? yU.extend({}, rx) : {
                    complete: Tb || !Tb && ZA || Ze(rx) && rx,
                    duration: rx,
                    easing: Tb && ZA || ZA && !Ze(ZA) && ZA
                };
                if (yU.fx.off)
                    wB.duration = 0;
                else if (typeof wB.duration !== "number")
                    if (wB.duration in yU.fx.speeds)
                        wB.duration = yU.fx.speeds[wB.duration];
                    else
                        wB.duration = yU.fx.speeds._default;
                if (wB.queue == null || wB.queue === true)
                    wB.queue = "fx";
                return wB.old = wB.complete,
                wB.complete = function() {
                    if (Ze(wB.old))
                        wB.old.call(this);
                    if (wB.queue)
                        yU.dequeue(this, wB.queue)
                }
                ,
                wB
            }
            ,
            yU.fn.extend({
                fadeTo: function(rx, ZA, Tb, wB) {
                    return this.filter(bs).css("opacity", 0).show().end().animate({
                        opacity: ZA
                    }, rx, Tb, wB)
                },
                animate: function(rx, ZA, Tb, wB) {
                    var lo = yU.isEmptyObject(rx)
                      , zC = yU.speed(ZA, Tb, wB)
                      , uA = function() {
                        var ZA = we(this, yU.extend({}, rx), zC);
                        if (lo || tv.get(this, "finish"))
                            ZA.stop(true)
                    };
                    return uA.finish = uA,
                    lo || zC.queue === false ? this.each(uA) : this.queue(zC.queue, uA)
                },
                stop: function(rx, ZA, Tb) {
                    var wB = function(rx) {
                        var ZA = rx.stop;
                        delete rx.stop,
                        ZA(Tb)
                    };
                    if (typeof rx !== "string")
                        Tb = ZA,
                        ZA = rx,
                        rx = void 0;
                    if (ZA && rx !== false)
                        this.queue(rx || "fx", []);
                    return this.each((function() {
                        var ZA = true
                          , lo = rx != null && rx + "queueHooks"
                          , zC = yU.timers
                          , uA = tv.get(this);
                        if (lo) {
                            if (uA[lo] && uA[lo].stop)
                                wB(uA[lo])
                        } else
                            for (lo in uA)
                                if (uA[lo] && uA[lo].stop && Zj.test(lo))
                                    wB(uA[lo]);
                        for (lo = zC.length; lo--; )
                            if (zC[lo].elem === this && (rx == null || zC[lo].queue === rx))
                                zC[lo].anim.stop(Tb),
                                ZA = false,
                                zC.splice(lo, 1);
                        if (ZA || !Tb)
                            yU.dequeue(this, rx)
                    }
                    ))
                },
                finish: function(rx) {
                    if (rx !== false)
                        rx = rx || "fx";
                    return this.each((function() {
                        var ZA, Tb = tv.get(this), wB = Tb[rx + "queue"], lo = Tb[rx + "queueHooks"], zC = yU.timers, uA = wB ? wB.length : 0;
                        if (Tb.finish = true,
                        yU.queue(this, rx, []),
                        lo && lo.stop)
                            lo.stop.call(this, true);
                        for (ZA = zC.length; ZA--; )
                            if (zC[ZA].elem === this && zC[ZA].queue === rx)
                                zC[ZA].anim.stop(true),
                                zC.splice(ZA, 1);
                        for (ZA = 0; ZA < uA; ZA++)
                            if (wB[ZA] && wB[ZA].finish)
                                wB[ZA].finish.call(this);
                        delete Tb.finish
                    }
                    ))
                }
            }),
            yU.each(["toggle", "show", "hide"], (function(rx, ZA) {
                var Tb = yU.fn[ZA];
                yU.fn[ZA] = function(rx, wB, lo) {
                    return rx == null || typeof rx === "boolean" ? Tb.apply(this, arguments) : this.animate(sJ(ZA, true), rx, wB, lo)
                }
            }
            )),
            yU.each({
                slideDown: sJ("show"),
                slideUp: sJ("hide"),
                slideToggle: sJ("toggle"),
                fadeIn: {
                    opacity: "show"
                },
                fadeOut: {
                    opacity: "hide"
                },
                fadeToggle: {
                    opacity: "toggle"
                }
            }, (function(rx, ZA) {
                yU.fn[rx] = function(rx, Tb, wB) {
                    return this.animate(ZA, rx, Tb, wB)
                }
            }
            )),
            yU.timers = [],
            yU.fx.tick = function() {
                var rx, ZA = 0, Tb = yU.timers;
                for (bt = Date.now(); ZA < Tb.length; ZA++)
                    if (rx = Tb[ZA],
                    !rx() && Tb[ZA] === rx)
                        Tb.splice(ZA--, 1);
                if (!Tb.length)
                    yU.fx.stop();
                bt = void 0
            }
            ,
            yU.fx.timer = function(rx) {
                yU.timers.push(rx),
                yU.fx.start()
            }
            ,
            yU.fx.interval = 13,
            yU.fx.start = function() {
                if (hX)
                    return;
                hX = true,
                oH()
            }
            ,
            yU.fx.stop = function() {
                hX = null
            }
            ,
            yU.fx.speeds = {
                slow: 600,
                fast: 200,
                _default: 400
            },
            yU.fn.delay = function(ZA, Tb) {
                return ZA = yU.fx ? yU.fx.speeds[ZA] || ZA : ZA,
                Tb = Tb || "fx",
                this.queue(Tb, (function(Tb, wB) {
                    var lo = rx.setTimeout(Tb, ZA);
                    wB.stop = function() {
                        rx.clearTimeout(lo)
                    }
                }
                ))
            }
            ,
            function() {
                var rx = wB.createElement("input")
                  , ZA = wB.createElement("select")
                  , Tb = ZA.appendChild(wB.createElement("option"));
                rx.type = "checkbox",
                ny.checkOn = rx.value !== "",
                ny.optSelected = Tb.selected,
                rx = wB.createElement("input"),
                rx.value = "t",
                rx.type = "radio",
                ny.radioValue = rx.value === "t"
            }();
            var mZ, Rf = yU.expr.attrHandle;
            yU.fn.extend({
                attr: function(rx, ZA) {
                    return wJ(this, yU.attr, rx, ZA, arguments.length > 1)
                },
                removeAttr: function(rx) {
                    return this.each((function() {
                        yU.removeAttr(this, rx)
                    }
                    ))
                }
            }),
            yU.extend({
                attr: function(rx, ZA, Tb) {
                    var wB, lo, zC = rx.nodeType;
                    if (zC === 3 || zC === 8 || zC === 2)
                        return;
                    if (typeof rx.getAttribute === "undefined")
                        return yU.prop(rx, ZA, Tb);
                    if (zC !== 1 || !yU.isXMLDoc(rx))
                        lo = yU.attrHooks[ZA.toLowerCase()] || (yU.expr.match.bool.test(ZA) ? mZ : void 0);
                    if (Tb !== void 0) {
                        if (Tb === null)
                            return void yU.removeAttr(rx, ZA);
                        if (lo && "set" in lo && (wB = lo.set(rx, Tb, ZA)) !== void 0)
                            return wB;
                        return rx.setAttribute(ZA, Tb + ""),
                        Tb
                    }
                    if (lo && "get" in lo && (wB = lo.get(rx, ZA)) !== null)
                        return wB;
                    return wB = yU.find.attr(rx, ZA),
                    wB == null ? void 0 : wB
                },
                attrHooks: {
                    type: {
                        set: function(rx, ZA) {
                            if (!ny.radioValue && ZA === "radio" && UI(rx, "input")) {
                                var Tb = rx.value;
                                if (rx.setAttribute("type", ZA),
                                Tb)
                                    rx.value = Tb;
                                return ZA
                            }
                        }
                    }
                },
                removeAttr: function(rx, ZA) {
                    var Tb, wB = 0, lo = ZA && ZA.match(oD);
                    if (lo && rx.nodeType === 1)
                        while (Tb = lo[wB++])
                            rx.removeAttribute(Tb)
                }
            }),
            mZ = {
                set: function(rx, ZA, Tb) {
                    if (ZA === false)
                        yU.removeAttr(rx, Tb);
                    else
                        rx.setAttribute(Tb, Tb);
                    return Tb
                }
            },
            yU.each(yU.expr.match.bool.source.match(/\w+/g), (function(rx, ZA) {
                var Tb = Rf[ZA] || yU.find.attr;
                Rf[ZA] = function(rx, ZA, wB) {
                    var lo, zC, uA = ZA.toLowerCase();
                    if (!wB)
                        zC = Rf[uA],
                        Rf[uA] = lo,
                        lo = Tb(rx, ZA, wB) != null ? uA : null,
                        Rf[uA] = zC;
                    return lo
                }
            }
            ));
            var kX = /^(?:input|select|textarea|button)$/i
              , JV = /^(?:a|area)$/i;
            if (yU.fn.extend({
                prop: function(rx, ZA) {
                    return wJ(this, yU.prop, rx, ZA, arguments.length > 1)
                },
                removeProp: function(rx) {
                    return this.each((function() {
                        delete this[yU.propFix[rx] || rx]
                    }
                    ))
                }
            }),
            yU.extend({
                prop: function(rx, ZA, Tb) {
                    var wB, lo, zC = rx.nodeType;
                    if (zC === 3 || zC === 8 || zC === 2)
                        return;
                    if (zC !== 1 || !yU.isXMLDoc(rx))
                        ZA = yU.propFix[ZA] || ZA,
                        lo = yU.propHooks[ZA];
                    if (Tb !== void 0) {
                        if (lo && "set" in lo && (wB = lo.set(rx, Tb, ZA)) !== void 0)
                            return wB;
                        return rx[ZA] = Tb
                    }
                    if (lo && "get" in lo && (wB = lo.get(rx, ZA)) !== null)
                        return wB;
                    return rx[ZA]
                },
                propHooks: {
                    tabIndex: {
                        get: function(rx) {
                            var ZA = yU.find.attr(rx, "tabindex");
                            if (ZA)
                                return parseInt(ZA, 10);
                            if (kX.test(rx.nodeName) || JV.test(rx.nodeName) && rx.href)
                                return 0;
                            return -1
                        }
                    }
                },
                propFix: {
                    for: "htmlFor",
                    class: "className"
                }
            }),
            !ny.optSelected)
                yU.propHooks.selected = {
                    get: function(rx) {
                        var ZA = rx.parentNode;
                        if (ZA && ZA.parentNode)
                            ZA.parentNode.selectedIndex;
                        return null
                    },
                    set: function(rx) {
                        var ZA = rx.parentNode;
                        if (ZA)
                            if (ZA.selectedIndex,
                            ZA.parentNode)
                                ZA.parentNode.selectedIndex
                    }
                };
            function wM(rx) {
                var ZA = rx.match(oD) || [];
                return ZA.join(" ")
            }
            function Qi(rx) {
                return rx.getAttribute && rx.getAttribute("class") || ""
            }
            function VW(rx) {
                if (Array.isArray(rx))
                    return rx;
                if (typeof rx === "string")
                    return rx.match(oD) || [];
                return []
            }
            yU.each(["tabIndex", "readOnly", "maxLength", "cellSpacing", "cellPadding", "rowSpan", "colSpan", "useMap", "frameBorder", "contentEditable"], (function() {
                yU.propFix[this.toLowerCase()] = this
            }
            )),
            yU.fn.extend({
                addClass: function(rx) {
                    var ZA, Tb, wB, lo, zC, uA, XH, OE = 0;
                    if (Ze(rx))
                        return this.each((function(ZA) {
                            yU(this).addClass(rx.call(this, ZA, Qi(this)))
                        }
                        ));
                    if (ZA = VW(rx),
                    ZA.length)
                        while (Tb = this[OE++])
                            if (lo = Qi(Tb),
                            wB = Tb.nodeType === 1 && " " + wM(lo) + " ",
                            wB) {
                                uA = 0;
                                while (zC = ZA[uA++])
                                    if (wB.indexOf(" " + zC + " ") < 0)
                                        wB += zC + " ";
                                if (XH = wM(wB),
                                lo !== XH)
                                    Tb.setAttribute("class", XH)
                            }
                    return this
                },
                removeClass: function(rx) {
                    var ZA, Tb, wB, lo, zC, uA, XH, OE = 0;
                    if (Ze(rx))
                        return this.each((function(ZA) {
                            yU(this).removeClass(rx.call(this, ZA, Qi(this)))
                        }
                        ));
                    if (!arguments.length)
                        return this.attr("class", "");
                    if (ZA = VW(rx),
                    ZA.length)
                        while (Tb = this[OE++])
                            if (lo = Qi(Tb),
                            wB = Tb.nodeType === 1 && " " + wM(lo) + " ",
                            wB) {
                                uA = 0;
                                while (zC = ZA[uA++])
                                    while (wB.indexOf(" " + zC + " ") > -1)
                                        wB = wB.replace(" " + zC + " ", " ");
                                if (XH = wM(wB),
                                lo !== XH)
                                    Tb.setAttribute("class", XH)
                            }
                    return this
                },
                toggleClass: function(rx, ZA) {
                    var Tb = typeof rx
                      , wB = Tb === "string" || Array.isArray(rx);
                    if (typeof ZA === "boolean" && wB)
                        return ZA ? this.addClass(rx) : this.removeClass(rx);
                    if (Ze(rx))
                        return this.each((function(Tb) {
                            yU(this).toggleClass(rx.call(this, Tb, Qi(this), ZA), ZA)
                        }
                        ));
                    return this.each((function() {
                        var ZA, lo, zC, uA;
                        if (wB) {
                            lo = 0,
                            zC = yU(this),
                            uA = VW(rx);
                            while (ZA = uA[lo++])
                                if (zC.hasClass(ZA))
                                    zC.removeClass(ZA);
                                else
                                    zC.addClass(ZA)
                        } else if (rx === void 0 || Tb === "boolean") {
                            if (ZA = Qi(this),
                            ZA)
                                tv.set(this, "__className__", ZA);
                            if (this.setAttribute)
                                this.setAttribute("class", ZA || rx === false ? "" : tv.get(this, "__className__") || "")
                        }
                    }
                    ))
                },
                hasClass: function(rx) {
                    var ZA, Tb, wB = 0;
                    ZA = " " + rx + " ";
                    while (Tb = this[wB++])
                        if (Tb.nodeType === 1 && (" " + wM(Qi(Tb)) + " ").indexOf(ZA) > -1)
                            return true;
                    return false
                }
            });
            var bn = /\r/g;
            yU.fn.extend({
                val: function(rx) {
                    var ZA, Tb, wB, lo = this[0];
                    if (!arguments.length) {
                        if (lo) {
                            if (ZA = yU.valHooks[lo.type] || yU.valHooks[lo.nodeName.toLowerCase()],
                            ZA && "get" in ZA && (Tb = ZA.get(lo, "value")) !== void 0)
                                return Tb;
                            if (Tb = lo.value,
                            typeof Tb === "string")
                                return Tb.replace(bn, "");
                            return Tb == null ? "" : Tb
                        }
                        return
                    }
                    return wB = Ze(rx),
                    this.each((function(Tb) {
                        var lo;
                        if (this.nodeType !== 1)
                            return;
                        if (wB)
                            lo = rx.call(this, Tb, yU(this).val());
                        else
                            lo = rx;
                        if (lo == null)
                            lo = "";
                        else if (typeof lo === "number")
                            lo += "";
                        else if (Array.isArray(lo))
                            lo = yU.map(lo, (function(rx) {
                                return rx == null ? "" : rx + ""
                            }
                            ));
                        if (ZA = yU.valHooks[this.type] || yU.valHooks[this.nodeName.toLowerCase()],
                        !ZA || !("set" in ZA) || ZA.set(this, lo, "value") === void 0)
                            this.value = lo
                    }
                    ))
                }
            }),
            yU.extend({
                valHooks: {
                    option: {
                        get: function(rx) {
                            var ZA = yU.find.attr(rx, "value");
                            return ZA != null ? ZA : wM(yU.text(rx))
                        }
                    },
                    select: {
                        get: function(rx) {
                            var ZA, Tb, wB, lo = rx.options, zC = rx.selectedIndex, uA = rx.type === "select-one", XH = uA ? null : [], OE = uA ? zC + 1 : lo.length;
                            if (zC < 0)
                                wB = OE;
                            else
                                wB = uA ? zC : 0;
                            for (; wB < OE; wB++)
                                if (Tb = lo[wB],
                                (Tb.selected || wB === zC) && !Tb.disabled && (!Tb.parentNode.disabled || !UI(Tb.parentNode, "optgroup"))) {
                                    if (ZA = yU(Tb).val(),
                                    uA)
                                        return ZA;
                                    XH.push(ZA)
                                }
                            return XH
                        },
                        set: function(rx, ZA) {
                            var Tb, wB, lo = rx.options, zC = yU.makeArray(ZA), uA = lo.length;
                            while (uA--)
                                if (wB = lo[uA],
                                wB.selected = yU.inArray(yU.valHooks.option.get(wB), zC) > -1)
                                    Tb = true;
                            if (!Tb)
                                rx.selectedIndex = -1;
                            return zC
                        }
                    }
                }
            }),
            yU.each(["radio", "checkbox"], (function() {
                if (yU.valHooks[this] = {
                    set: function(rx, ZA) {
                        if (Array.isArray(ZA))
                            return rx.checked = yU.inArray(yU(rx).val(), ZA) > -1
                    }
                },
                !ny.checkOn)
                    yU.valHooks[this].get = function(rx) {
                        return rx.getAttribute("value") === null ? "on" : rx.value
                    }
            }
            )),
            ny.focusin = "onfocusin" in rx;
            var qm = /^(?:focusinfocus|focusoutblur)$/
              , UM = function(rx) {
                rx.stopPropagation()
            };
            if (yU.extend(yU.event, {
                trigger: function(ZA, Tb, lo, zC) {
                    var uA, XH, OE, Us, ZQ, SO, yi, ny, Fc = [lo || wB], yI = VX.call(ZA, "type") ? ZA.type : ZA, jG = VX.call(ZA, "namespace") ? ZA.namespace.split(".") : [];
                    if (XH = ny = OE = lo = lo || wB,
                    lo.nodeType === 3 || lo.nodeType === 8)
                        return;
                    if (qm.test(yI + yU.event.triggered))
                        return;
                    if (yI.indexOf(".") > -1)
                        jG = yI.split("."),
                        yI = jG.shift(),
                        jG.sort();
                    if (ZQ = yI.indexOf(":") < 0 && "on" + yI,
                    ZA = ZA[yU.expando] ? ZA : new yU.Event(yI,typeof ZA === "object" && ZA),
                    ZA.isTrigger = zC ? 2 : 3,
                    ZA.namespace = jG.join("."),
                    ZA.rnamespace = ZA.namespace ? new RegExp("(^|\\.)" + jG.join("\\.(?:.*\\.|)") + "(\\.|$)") : null,
                    ZA.result = void 0,
                    !ZA.target)
                        ZA.target = lo;
                    if (Tb = Tb == null ? [ZA] : yU.makeArray(Tb, [ZA]),
                    yi = yU.event.special[yI] || {},
                    !zC && yi.trigger && yi.trigger.apply(lo, Tb) === false)
                        return;
                    if (!zC && !yi.noBubble && !zO(lo)) {
                        if (Us = yi.delegateType || yI,
                        !qm.test(Us + yI))
                            XH = XH.parentNode;
                        for (; XH; XH = XH.parentNode)
                            Fc.push(XH),
                            OE = XH;
                        if (OE === (lo.ownerDocument || wB))
                            Fc.push(OE.defaultView || OE.parentWindow || rx)
                    }
                    uA = 0;
                    while ((XH = Fc[uA++]) && !ZA.isPropagationStopped()) {
                        if (ny = XH,
                        ZA.type = uA > 1 ? Us : yi.bindType || yI,
                        SO = (tv.get(XH, "events") || {})[ZA.type] && tv.get(XH, "handle"),
                        SO)
                            SO.apply(XH, Tb);
                        if (SO = ZQ && XH[ZQ],
                        SO && SO.apply && uH(XH))
                            if (ZA.result = SO.apply(XH, Tb),
                            ZA.result === false)
                                ZA.preventDefault()
                    }
                    if (ZA.type = yI,
                    !zC && !ZA.isDefaultPrevented())
                        if ((!yi._default || yi._default.apply(Fc.pop(), Tb) === false) && uH(lo))
                            if (ZQ && Ze(lo[yI]) && !zO(lo)) {
                                if (OE = lo[ZQ],
                                OE)
                                    lo[ZQ] = null;
                                if (yU.event.triggered = yI,
                                ZA.isPropagationStopped())
                                    ny.addEventListener(yI, UM);
                                if (lo[yI](),
                                ZA.isPropagationStopped())
                                    ny.removeEventListener(yI, UM);
                                if (yU.event.triggered = void 0,
                                OE)
                                    lo[ZQ] = OE
                            }
                    return ZA.result
                },
                simulate: function(rx, ZA, Tb) {
                    var wB = yU.extend(new yU.Event, Tb, {
                        type: rx,
                        isSimulated: true
                    });
                    yU.event.trigger(wB, null, ZA)
                }
            }),
            yU.fn.extend({
                trigger: function(rx, ZA) {
                    return this.each((function() {
                        yU.event.trigger(rx, ZA, this)
                    }
                    ))
                },
                triggerHandler: function(rx, ZA) {
                    var Tb = this[0];
                    if (Tb)
                        return yU.event.trigger(rx, ZA, Tb, true)
                }
            }),
            !ny.focusin)
                yU.each({
                    focus: "focusin",
                    blur: "focusout"
                }, (function(rx, ZA) {
                    var Tb = function(rx) {
                        yU.event.simulate(ZA, rx.target, yU.event.fix(rx))
                    };
                    yU.event.special[ZA] = {
                        setup: function() {
                            var wB = this.ownerDocument || this
                              , lo = tv.access(wB, ZA);
                            if (!lo)
                                wB.addEventListener(rx, Tb, true);
                            tv.access(wB, ZA, (lo || 0) + 1)
                        },
                        teardown: function() {
                            var wB = this.ownerDocument || this
                              , lo = tv.access(wB, ZA) - 1;
                            if (!lo)
                                wB.removeEventListener(rx, Tb, true),
                                tv.remove(wB, ZA);
                            else
                                tv.access(wB, ZA, lo)
                        }
                    }
                }
                ));
            var Up = rx.location
              , jV = Date.now()
              , jt = /\?/;
            yU.parseXML = function(ZA) {
                var Tb;
                if (!ZA || typeof ZA !== "string")
                    return null;
                try {
                    Tb = (new rx.DOMParser).parseFromString(ZA, "text/xml")
                } catch (rx) {
                    Tb = void 0
                }
                if (!Tb || Tb.getElementsByTagName("parsererror").length)
                    yU.error("Invalid XML: " + ZA);
                return Tb
            }
            ;
            var gt = /\[\]$/
              , XG = /\r?\n/g
              , PW = /^(?:submit|button|image|reset|file)$/i
              , Vs = /^(?:input|select|textarea|keygen)/i;
            function DM(rx, ZA, Tb, wB) {
                var lo;
                if (Array.isArray(ZA))
                    yU.each(ZA, (function(ZA, lo) {
                        if (Tb || gt.test(rx))
                            wB(rx, lo);
                        else
                            DM(rx + "[" + (typeof lo === "object" && lo != null ? ZA : "") + "]", lo, Tb, wB)
                    }
                    ));
                else if (!Tb && jG(ZA) === "object")
                    for (lo in ZA)
                        DM(rx + "[" + lo + "]", ZA[lo], Tb, wB);
                else
                    wB(rx, ZA)
            }
            yU.param = function(rx, ZA) {
                var Tb, wB = [], lo = function(rx, ZA) {
                    var Tb = Ze(ZA) ? ZA() : ZA;
                    wB[wB.length] = encodeURIComponent(rx) + "=" + encodeURIComponent(Tb == null ? "" : Tb)
                };
                if (rx == null)
                    return "";
                if (Array.isArray(rx) || rx.jquery && !yU.isPlainObject(rx))
                    yU.each(rx, (function() {
                        lo(this.name, this.value)
                    }
                    ));
                else
                    for (Tb in rx)
                        DM(Tb, rx[Tb], ZA, lo);
                return wB.join("&")
            }
            ,
            yU.fn.extend({
                serialize: function() {
                    return yU.param(this.serializeArray())
                },
                serializeArray: function() {
                    return this.map((function() {
                        var rx = yU.prop(this, "elements");
                        return rx ? yU.makeArray(rx) : this
                    }
                    )).filter((function() {
                        var rx = this.type;
                        return this.name && !yU(this).is(":disabled") && Vs.test(this.nodeName) && !PW.test(rx) && (this.checked || !Kd.test(rx))
                    }
                    )).map((function(rx, ZA) {
                        var Tb = yU(this).val();
                        if (Tb == null)
                            return null;
                        if (Array.isArray(Tb))
                            return yU.map(Tb, (function(rx) {
                                return {
                                    name: ZA.name,
                                    value: rx.replace(XG, "\r\n")
                                }
                            }
                            ));
                        return {
                            name: ZA.name,
                            value: Tb.replace(XG, "\r\n")
                        }
                    }
                    )).get()
                }
            });
            var Qw = /%20/g
              , Xj = /#.*$/
              , rB = /([?&])_=[^&]*/
              , df = /^(.*?):[ \t]*([^\r\n]*)$/gm
              , qC = /^(?:about|app|app-storage|.+-extension|file|res|widget):$/
              , mN = /^(?:GET|HEAD)$/
              , wn = /^\/\//
              , kp = {}
              , Lx = {}
              , aP = "*/".concat("*")
              , Mt = wB.createElement("a");
            function Sw(rx) {
                return function(ZA, Tb) {
                    if (typeof ZA !== "string")
                        Tb = ZA,
                        ZA = "*";
                    var wB, lo = 0, zC = ZA.toLowerCase().match(oD) || [];
                    if (Ze(Tb))
                        while (wB = zC[lo++])
                            if (wB[0] === "+")
                                wB = wB.slice(1) || "*",
                                (rx[wB] = rx[wB] || []).unshift(Tb);
                            else
                                (rx[wB] = rx[wB] || []).push(Tb)
                }
            }
            function pJ(rx, ZA, Tb, wB) {
                var lo = {}
                  , zC = rx === Lx;
                function uA(XH) {
                    var OE;
                    return lo[XH] = true,
                    yU.each(rx[XH] || [], (function(rx, XH) {
                        var Us = XH(ZA, Tb, wB);
                        if (typeof Us === "string" && !zC && !lo[Us])
                            return ZA.dataTypes.unshift(Us),
                            uA(Us),
                            false;
                        else if (zC)
                            return !(OE = Us)
                    }
                    )),
                    OE
                }
                return uA(ZA.dataTypes[0]) || !lo["*"] && uA("*")
            }
            function BF(rx, ZA) {
                var Tb, wB, lo = yU.ajaxSettings.flatOptions || {};
                for (Tb in ZA)
                    if (ZA[Tb] !== void 0)
                        (lo[Tb] ? rx : wB || (wB = {}))[Tb] = ZA[Tb];
                if (wB)
                    yU.extend(true, rx, wB);
                return rx
            }
            function Pn(rx, ZA, Tb) {
                var wB, lo, zC, uA, XH = rx.contents, OE = rx.dataTypes;
                while (OE[0] === "*")
                    if (OE.shift(),
                    wB === void 0)
                        wB = rx.mimeType || ZA.getResponseHeader("Content-Type");
                if (wB)
                    for (lo in XH)
                        if (XH[lo] && XH[lo].test(wB)) {
                            OE.unshift(lo);
                            break
                        }
                if (OE[0] in Tb)
                    zC = OE[0];
                else {
                    for (lo in Tb) {
                        if (!OE[0] || rx.converters[lo + " " + OE[0]]) {
                            zC = lo;
                            break
                        }
                        if (!uA)
                            uA = lo
                    }
                    zC = zC || uA
                }
                if (zC) {
                    if (zC !== OE[0])
                        OE.unshift(zC);
                    return Tb[zC]
                }
            }
            function vg(rx, ZA, Tb, wB) {
                var lo, zC, uA, XH, OE, Us = {}, ZQ = rx.dataTypes.slice();
                if (ZQ[1])
                    for (uA in rx.converters)
                        Us[uA.toLowerCase()] = rx.converters[uA];
                zC = ZQ.shift();
                while (zC) {
                    if (rx.responseFields[zC])
                        Tb[rx.responseFields[zC]] = ZA;
                    if (!OE && wB && rx.dataFilter)
                        ZA = rx.dataFilter(ZA, rx.dataType);
                    if (OE = zC,
                    zC = ZQ.shift(),
                    zC)
                        if (zC === "*")
                            zC = OE;
                        else if (OE !== "*" && OE !== zC) {
                            if (uA = Us[OE + " " + zC] || Us["* " + zC],
                            !uA)
                                for (lo in Us)
                                    if (XH = lo.split(" "),
                                    XH[1] === zC)
                                        if (uA = Us[OE + " " + XH[0]] || Us["* " + XH[0]],
                                        uA) {
                                            if (uA === true)
                                                uA = Us[lo];
                                            else if (Us[lo] !== true)
                                                zC = XH[0],
                                                ZQ.unshift(XH[1]);
                                            break
                                        }
                            if (uA !== true)
                                if (uA && rx.throws)
                                    ZA = uA(ZA);
                                else
                                    try {
                                        ZA = uA(ZA)
                                    } catch (rx) {
                                        return {
                                            state: "parsererror",
                                            error: uA ? rx : "No conversion from " + OE + " to " + zC
                                        }
                                    }
                        }
                }
                return {
                    state: "success",
                    data: ZA
                }
            }
            Mt.href = Up.href,
            yU.extend({
                active: 0,
                lastModified: {},
                etag: {},
                ajaxSettings: {
                    url: Up.href,
                    type: "GET",
                    isLocal: qC.test(Up.protocol),
                    global: true,
                    processData: true,
                    async: true,
                    contentType: "application/x-www-form-urlencoded; charset=UTF-8",
                    accepts: {
                        "*": aP,
                        text: "text/plain",
                        html: "text/html",
                        xml: "application/xml, text/xml",
                        json: "application/json, text/javascript"
                    },
                    contents: {
                        xml: /\bxml\b/,
                        html: /\bhtml/,
                        json: /\bjson\b/
                    },
                    responseFields: {
                        xml: "responseXML",
                        text: "responseText",
                        json: "responseJSON"
                    },
                    converters: {
                        "* text": String,
                        "text html": true,
                        "text json": JSON.parse,
                        "text xml": yU.parseXML
                    },
                    flatOptions: {
                        url: true,
                        context: true
                    }
                },
                ajaxSetup: function(rx, ZA) {
                    return ZA ? BF(BF(rx, yU.ajaxSettings), ZA) : BF(yU.ajaxSettings, rx)
                },
                ajaxPrefilter: Sw(kp),
                ajaxTransport: Sw(Lx),
                ajax: function(ZA, Tb) {
                    if (typeof ZA === "object")
                        Tb = ZA,
                        ZA = void 0;
                    Tb = Tb || {};
                    var lo, zC, uA, XH, OE, Us, ZQ, VX, SO, yi, ny = yU.ajaxSetup({}, Tb), Ze = ny.context || ny, zO = ny.context && (Ze.nodeType || Ze.jquery) ? yU(Ze) : yU.event, Fc = yU.Deferred(), yI = yU.Callbacks("once memory"), jG = ny.statusCode || {}, XF = {}, QG = {}, dS = "canceled", FW = {
                        readyState: 0,
                        getResponseHeader: function(rx) {
                            var ZA;
                            if (ZQ) {
                                if (!XH) {
                                    XH = {};
                                    while (ZA = df.exec(uA))
                                        XH[ZA[1].toLowerCase() + " "] = (XH[ZA[1].toLowerCase() + " "] || []).concat(ZA[2])
                                }
                                ZA = XH[rx.toLowerCase() + " "]
                            }
                            return ZA == null ? null : ZA.join(", ")
                        },
                        getAllResponseHeaders: function() {
                            return ZQ ? uA : null
                        },
                        setRequestHeader: function(rx, ZA) {
                            if (ZQ == null)
                                rx = QG[rx.toLowerCase()] = QG[rx.toLowerCase()] || rx,
                                XF[rx] = ZA;
                            return this
                        },
                        overrideMimeType: function(rx) {
                            if (ZQ == null)
                                ny.mimeType = rx;
                            return this
                        },
                        statusCode: function(rx) {
                            var ZA;
                            if (rx)
                                if (ZQ)
                                    FW.always(rx[FW.status]);
                                else
                                    for (ZA in rx)
                                        jG[ZA] = [jG[ZA], rx[ZA]];
                            return this
                        },
                        abort: function(rx) {
                            var ZA = rx || dS;
                            if (lo)
                                lo.abort(ZA);
                            return nm(0, ZA),
                            this
                        }
                    };
                    if (Fc.promise(FW),
                    ny.url = ((ZA || ny.url || Up.href) + "").replace(wn, Up.protocol + "//"),
                    ny.type = Tb.method || Tb.type || ny.method || ny.type,
                    ny.dataTypes = (ny.dataType || "*").toLowerCase().match(oD) || [""],
                    ny.crossDomain == null) {
                        Us = wB.createElement("a");
                        try {
                            Us.href = ny.url,
                            Us.href = Us.href,
                            ny.crossDomain = Mt.protocol + "//" + Mt.host !== Us.protocol + "//" + Us.host
                        } catch (rx) {
                            ny.crossDomain = true
                        }
                    }
                    if (ny.data && ny.processData && typeof ny.data !== "string")
                        ny.data = yU.param(ny.data, ny.traditional);
                    if (pJ(kp, ny, Tb, FW),
                    ZQ)
                        return FW;
                    if (VX = yU.event && ny.global,
                    VX && yU.active++ === 0)
                        yU.event.trigger("ajaxStart");
                    if (ny.type = ny.type.toUpperCase(),
                    ny.hasContent = !mN.test(ny.type),
                    zC = ny.url.replace(Xj, ""),
                    !ny.hasContent) {
                        if (yi = ny.url.slice(zC.length),
                        ny.data && (ny.processData || typeof ny.data === "string"))
                            zC += (jt.test(zC) ? "&" : "?") + ny.data,
                            delete ny.data;
                        if (ny.cache === false)
                            zC = zC.replace(rB, "$1"),
                            yi = (jt.test(zC) ? "&" : "?") + "_=" + jV++ + yi;
                        ny.url = zC + yi
                    } else if (ny.data && ny.processData && (ny.contentType || "").indexOf("application/x-www-form-urlencoded") === 0)
                        ny.data = ny.data.replace(Qw, "+");
                    if (ny.ifModified) {
                        if (yU.lastModified[zC])
                            FW.setRequestHeader("If-Modified-Since", yU.lastModified[zC]);
                        if (yU.etag[zC])
                            FW.setRequestHeader("If-None-Match", yU.etag[zC])
                    }
                    if (ny.data && ny.hasContent && ny.contentType !== false || Tb.contentType)
                        FW.setRequestHeader("Content-Type", ny.contentType);
                    for (SO in FW.setRequestHeader("Accept", ny.dataTypes[0] && ny.accepts[ny.dataTypes[0]] ? ny.accepts[ny.dataTypes[0]] + (ny.dataTypes[0] !== "*" ? ", " + aP + "; q=0.01" : "") : ny.accepts["*"]),
                    ny.headers)
                        FW.setRequestHeader(SO, ny.headers[SO]);
                    if (ny.beforeSend && (ny.beforeSend.call(Ze, FW, ny) === false || ZQ))
                        return FW.abort();
                    if (dS = "abort",
                    yI.add(ny.complete),
                    FW.done(ny.success),
                    FW.fail(ny.error),
                    lo = pJ(Lx, ny, Tb, FW),
                    !lo)
                        nm(-1, "No Transport");
                    else {
                        if (FW.readyState = 1,
                        VX)
                            zO.trigger("ajaxSend", [FW, ny]);
                        if (ZQ)
                            return FW;
                        if (ny.async && ny.timeout > 0)
                            OE = rx.setTimeout((function() {
                                FW.abort("timeout")
                            }
                            ), ny.timeout);
                        try {
                            ZQ = false,
                            lo.send(XF, nm)
                        } catch (rx) {
                            if (ZQ)
                                throw rx;
                            nm(-1, rx)
                        }
                    }
                    function nm(ZA, Tb, wB, XH) {
                        var Us, SO, yi, XF, QG, dS = Tb;
                        if (ZQ)
                            return;
                        if (ZQ = true,
                        OE)
                            rx.clearTimeout(OE);
                        if (lo = void 0,
                        uA = XH || "",
                        FW.readyState = ZA > 0 ? 4 : 0,
                        Us = ZA >= 200 && ZA < 300 || ZA === 304,
                        wB)
                            XF = Pn(ny, FW, wB);
                        if (XF = vg(ny, XF, FW, Us),
                        Us) {
                            if (ny.ifModified) {
                                if (QG = FW.getResponseHeader("Last-Modified"),
                                QG)
                                    yU.lastModified[zC] = QG;
                                if (QG = FW.getResponseHeader("etag"),
                                QG)
                                    yU.etag[zC] = QG
                            }
                            if (ZA === 204 || ny.type === "HEAD")
                                dS = "nocontent";
                            else if (ZA === 304)
                                dS = "notmodified";
                            else
                                dS = XF.state,
                                SO = XF.data,
                                yi = XF.error,
                                Us = !yi
                        } else if (yi = dS,
                        ZA || !dS)
                            if (dS = "error",
                            ZA < 0)
                                ZA = 0;
                        if (FW.status = ZA,
                        FW.statusText = (Tb || dS) + "",
                        Us)
                            Fc.resolveWith(Ze, [SO, dS, FW]);
                        else
                            Fc.rejectWith(Ze, [FW, dS, yi]);
                        if (FW.statusCode(jG),
                        jG = void 0,
                        VX)
                            zO.trigger(Us ? "ajaxSuccess" : "ajaxError", [FW, ny, Us ? SO : yi]);
                        if (yI.fireWith(Ze, [FW, dS]),
                        VX)
                            if (zO.trigger("ajaxComplete", [FW, ny]),
                            !--yU.active)
                                yU.event.trigger("ajaxStop")
                    }
                    return FW
                },
                getJSON: function(rx, ZA, Tb) {
                    return yU.get(rx, ZA, Tb, "json")
                },
                getScript: function(rx, ZA) {
                    return yU.get(rx, void 0, ZA, "script")
                }
            }),
            yU.each(["get", "post"], (function(rx, ZA) {
                yU[ZA] = function(rx, Tb, wB, lo) {
                    if (Ze(Tb))
                        lo = lo || wB,
                        wB = Tb,
                        Tb = void 0;
                    return yU.ajax(yU.extend({
                        url: rx,
                        type: ZA,
                        dataType: lo,
                        data: Tb,
                        success: wB
                    }, yU.isPlainObject(rx) && rx))
                }
            }
            )),
            yU._evalUrl = function(rx, ZA) {
                return yU.ajax({
                    url: rx,
                    type: "GET",
                    dataType: "script",
                    cache: true,
                    async: false,
                    global: false,
                    converters: {
                        "text script": function() {}
                    },
                    dataFilter: function(rx) {
                        yU.globalEval(rx, ZA)
                    }
                })
            }
            ,
            yU.fn.extend({
                wrapAll: function(rx) {
                    var ZA;
                    if (this[0]) {
                        if (Ze(rx))
                            rx = rx.call(this[0]);
                        if (ZA = yU(rx, this[0].ownerDocument).eq(0).clone(true),
                        this[0].parentNode)
                            ZA.insertBefore(this[0]);
                        ZA.map((function() {
                            var rx = this;
                            while (rx.firstElementChild)
                                rx = rx.firstElementChild;
                            return rx
                        }
                        )).append(this)
                    }
                    return this
                },
                wrapInner: function(rx) {
                    if (Ze(rx))
                        return this.each((function(ZA) {
                            yU(this).wrapInner(rx.call(this, ZA))
                        }
                        ));
                    return this.each((function() {
                        var ZA = yU(this)
                          , Tb = ZA.contents();
                        if (Tb.length)
                            Tb.wrapAll(rx);
                        else
                            ZA.append(rx)
                    }
                    ))
                },
                wrap: function(rx) {
                    var ZA = Ze(rx);
                    return this.each((function(Tb) {
                        yU(this).wrapAll(ZA ? rx.call(this, Tb) : rx)
                    }
                    ))
                },
                unwrap: function(rx) {
                    return this.parent(rx).not("body").each((function() {
                        yU(this).replaceWith(this.childNodes)
                    }
                    )),
                    this
                }
            }),
            yU.expr.pseudos.hidden = function(rx) {
                return !yU.expr.pseudos.visible(rx)
            }
            ,
            yU.expr.pseudos.visible = function(rx) {
                return !!(rx.offsetWidth || rx.offsetHeight || rx.getClientRects().length)
            }
            ,
            yU.ajaxSettings.xhr = function() {
                try {
                    return new rx.XMLHttpRequest
                } catch (rx) {}
            }
            ;
            var DI = {
                0: 200,
                1223: 204
            }
              , fs = yU.ajaxSettings.xhr();
            ny.cors = !!fs && "withCredentials" in fs,
            ny.ajax = fs = !!fs,
            yU.ajaxTransport((function(ZA) {
                var Tb, wB;
                if (ny.cors || fs && !ZA.crossDomain)
                    return {
                        send: function(lo, zC) {
                            var uA, XH = ZA.xhr();
                            if (XH.open(ZA.type, ZA.url, ZA.async, ZA.username, ZA.password),
                            ZA.xhrFields)
                                for (uA in ZA.xhrFields)
                                    XH[uA] = ZA.xhrFields[uA];
                            if (ZA.mimeType && XH.overrideMimeType)
                                XH.overrideMimeType(ZA.mimeType);
                            if (!ZA.crossDomain && !lo["X-Requested-With"])
                                lo["X-Requested-With"] = "XMLHttpRequest";
                            for (uA in lo)
                                XH.setRequestHeader(uA, lo[uA]);
                            if (Tb = function(rx) {
                                return function() {
                                    if (Tb)
                                        if (Tb = wB = XH.onload = XH.onerror = XH.onabort = XH.ontimeout = XH.onreadystatechange = null,
                                        rx === "abort")
                                            XH.abort();
                                        else if (rx === "error")
                                            if (typeof XH.status !== "number")
                                                zC(0, "error");
                                            else
                                                zC(XH.status, XH.statusText);
                                        else
                                            zC(DI[XH.status] || XH.status, XH.statusText, (XH.responseType || "text") !== "text" || typeof XH.responseText !== "string" ? {
                                                binary: XH.response
                                            } : {
                                                text: XH.responseText
                                            }, XH.getAllResponseHeaders())
                                }
                            }
                            ,
                            XH.onload = Tb(),
                            wB = XH.onerror = XH.ontimeout = Tb("error"),
                            XH.onabort !== void 0)
                                XH.onabort = wB;
                            else
                                XH.onreadystatechange = function() {
                                    if (XH.readyState === 4)
                                        rx.setTimeout((function() {
                                            if (Tb)
                                                wB()
                                        }
                                        ))
                                }
                                ;
                            Tb = Tb("abort");
                            try {
                                XH.send(ZA.hasContent && ZA.data || null)
                            } catch (rx) {
                                if (Tb)
                                    throw rx
                            }
                        },
                        abort: function() {
                            if (Tb)
                                Tb()
                        }
                    }
            }
            )),
            yU.ajaxPrefilter((function(rx) {
                if (rx.crossDomain)
                    rx.contents.script = false
            }
            )),
            yU.ajaxSetup({
                accepts: {
                    script: "text/javascript, application/javascript, " + "application/ecmascript, application/x-ecmascript"
                },
                contents: {
                    script: /\b(?:java|ecma)script\b/
                },
                converters: {
                    "text script": function(rx) {
                        return yU.globalEval(rx),
                        rx
                    }
                }
            }),
            yU.ajaxPrefilter("script", (function(rx) {
                if (rx.cache === void 0)
                    rx.cache = false;
                if (rx.crossDomain)
                    rx.type = "GET"
            }
            )),
            yU.ajaxTransport("script", (function(rx) {
                if (rx.crossDomain || rx.scriptAttrs) {
                    var ZA, Tb;
                    return {
                        send: function(lo, zC) {
                            ZA = yU("<script>").attr(rx.scriptAttrs || {}).prop({
                                charset: rx.scriptCharset,
                                src: rx.url
                            }).on("load error", Tb = function(rx) {
                                if (ZA.remove(),
                                Tb = null,
                                rx)
                                    zC(rx.type === "error" ? 404 : 200, rx.type)
                            }
                            ),
                            wB.head.appendChild(ZA[0])
                        },
                        abort: function() {
                            if (Tb)
                                Tb()
                        }
                    }
                }
            }
            ));
            var sX = [], pE = /(=)\?(?=&|$)|\?\?/, sF;
            if (yU.ajaxSetup({
                jsonp: "callback",
                jsonpCallback: function() {
                    var rx = sX.pop() || yU.expando + "_" + jV++;
                    return this[rx] = true,
                    rx
                }
            }),
            yU.ajaxPrefilter("json jsonp", (function(ZA, Tb, wB) {
                var lo, zC, uA, XH = ZA.jsonp !== false && (pE.test(ZA.url) ? "url" : typeof ZA.data === "string" && (ZA.contentType || "").indexOf("application/x-www-form-urlencoded") === 0 && pE.test(ZA.data) && "data");
                if (XH || ZA.dataTypes[0] === "jsonp") {
                    if (lo = ZA.jsonpCallback = Ze(ZA.jsonpCallback) ? ZA.jsonpCallback() : ZA.jsonpCallback,
                    XH)
                        ZA[XH] = ZA[XH].replace(pE, "$1" + lo);
                    else if (ZA.jsonp !== false)
                        ZA.url += (jt.test(ZA.url) ? "&" : "?") + ZA.jsonp + "=" + lo;
                    return ZA.converters["script json"] = function() {
                        if (!uA)
                            yU.error(lo + " was not called");
                        return uA[0]
                    }
                    ,
                    ZA.dataTypes[0] = "json",
                    zC = rx[lo],
                    rx[lo] = function() {
                        uA = arguments
                    }
                    ,
                    wB.always((function() {
                        if (zC === void 0)
                            yU(rx).removeProp(lo);
                        else
                            rx[lo] = zC;
                        if (ZA[lo])
                            ZA.jsonpCallback = Tb.jsonpCallback,
                            sX.push(lo);
                        if (uA && Ze(zC))
                            zC(uA[0]);
                        uA = zC = void 0
                    }
                    )),
                    "script"
                }
            }
            )),
            ny.createHTMLDocument = (sF = wB.implementation.createHTMLDocument("").body,
            sF.innerHTML = "<form></form><form></form>",
            sF.childNodes.length === 2),
            yU.parseHTML = function(rx, ZA, Tb) {
                if (typeof rx !== "string")
                    return [];
                if (typeof ZA === "boolean")
                    Tb = ZA,
                    ZA = false;
                var lo, zC, uA;
                if (!ZA)
                    if (ny.createHTMLDocument)
                        ZA = wB.implementation.createHTMLDocument(""),
                        lo = ZA.createElement("base"),
                        lo.href = wB.location.href,
                        ZA.head.appendChild(lo);
                    else
                        ZA = wB;
                if (zC = iO.exec(rx),
                uA = !Tb && [],
                zC)
                    return [ZA.createElement(zC[1])];
                if (zC = NX([rx], ZA, uA),
                uA && uA.length)
                    yU(uA).remove();
                return yU.merge([], zC.childNodes)
            }
            ,
            yU.fn.load = function(rx, ZA, Tb) {
                var wB, lo, zC, uA = this, XH = rx.indexOf(" ");
                if (XH > -1)
                    wB = wM(rx.slice(XH)),
                    rx = rx.slice(0, XH);
                if (Ze(ZA))
                    Tb = ZA,
                    ZA = void 0;
                else if (ZA && typeof ZA === "object")
                    lo = "POST";
                if (uA.length > 0)
                    yU.ajax({
                        url: rx,
                        type: lo || "GET",
                        dataType: "html",
                        data: ZA
                    }).done((function(rx) {
                        zC = arguments,
                        uA.html(wB ? yU("<div>").append(yU.parseHTML(rx)).find(wB) : rx)
                    }
                    )).always(Tb && function(rx, ZA) {
                        uA.each((function() {
                            Tb.apply(this, zC || [rx.responseText, ZA, rx])
                        }
                        ))
                    }
                    );
                return this
            }
            ,
            yU.each(["ajaxStart", "ajaxStop", "ajaxComplete", "ajaxError", "ajaxSuccess", "ajaxSend"], (function(rx, ZA) {
                yU.fn[ZA] = function(rx) {
                    return this.on(ZA, rx)
                }
            }
            )),
            yU.expr.pseudos.animated = function(rx) {
                return yU.grep(yU.timers, (function(ZA) {
                    return rx === ZA.elem
                }
                )).length
            }
            ,
            yU.offset = {
                setOffset: function(rx, ZA, Tb) {
                    var wB, lo, zC, uA, XH, OE, Us, ZQ = yU.css(rx, "position"), VX = yU(rx), SO = {};
                    if (ZQ === "static")
                        rx.style.position = "relative";
                    if (XH = VX.offset(),
                    zC = yU.css(rx, "top"),
                    OE = yU.css(rx, "left"),
                    Us = (ZQ === "absolute" || ZQ === "fixed") && (zC + OE).indexOf("auto") > -1,
                    Us)
                        wB = VX.position(),
                        uA = wB.top,
                        lo = wB.left;
                    else
                        uA = parseFloat(zC) || 0,
                        lo = parseFloat(OE) || 0;
                    if (Ze(ZA))
                        ZA = ZA.call(rx, Tb, yU.extend({}, XH));
                    if (ZA.top != null)
                        SO.top = ZA.top - XH.top + uA;
                    if (ZA.left != null)
                        SO.left = ZA.left - XH.left + lo;
                    if ("using" in ZA)
                        ZA.using.call(rx, SO);
                    else
                        VX.css(SO)
                }
            },
            yU.fn.extend({
                offset: function(rx) {
                    if (arguments.length)
                        return rx === void 0 ? this : this.each((function(ZA) {
                            yU.offset.setOffset(this, rx, ZA)
                        }
                        ));
                    var ZA, Tb, wB = this[0];
                    if (!wB)
                        return;
                    if (!wB.getClientRects().length)
                        return {
                            top: 0,
                            left: 0
                        };
                    return ZA = wB.getBoundingClientRect(),
                    Tb = wB.ownerDocument.defaultView,
                    {
                        top: ZA.top + Tb.pageYOffset,
                        left: ZA.left + Tb.pageXOffset
                    }
                },
                position: function() {
                    if (!this[0])
                        return;
                    var rx, ZA, Tb, wB = this[0], lo = {
                        top: 0,
                        left: 0
                    };
                    if (yU.css(wB, "position") === "fixed")
                        ZA = wB.getBoundingClientRect();
                    else {
                        ZA = this.offset(),
                        Tb = wB.ownerDocument,
                        rx = wB.offsetParent || Tb.documentElement;
                        while (rx && (rx === Tb.body || rx === Tb.documentElement) && yU.css(rx, "position") === "static")
                            rx = rx.parentNode;
                        if (rx && rx !== wB && rx.nodeType === 1)
                            lo = yU(rx).offset(),
                            lo.top += yU.css(rx, "borderTopWidth", true),
                            lo.left += yU.css(rx, "borderLeftWidth", true)
                    }
                    return {
                        top: ZA.top - lo.top - yU.css(wB, "marginTop", true),
                        left: ZA.left - lo.left - yU.css(wB, "marginLeft", true)
                    }
                },
                offsetParent: function() {
                    return this.map((function() {
                        var rx = this.offsetParent;
                        while (rx && yU.css(rx, "position") === "static")
                            rx = rx.offsetParent;
                        return rx || nx
                    }
                    ))
                }
            }),
            yU.each({
                scrollLeft: "pageXOffset",
                scrollTop: "pageYOffset"
            }, (function(rx, ZA) {
                var Tb = "pageYOffset" === ZA;
                yU.fn[rx] = function(wB) {
                    return wJ(this, (function(rx, wB, lo) {
                        var zC;
                        if (zO(rx))
                            zC = rx;
                        else if (rx.nodeType === 9)
                            zC = rx.defaultView;
                        if (lo === void 0)
                            return zC ? zC[ZA] : rx[wB];
                        if (zC)
                            zC.scrollTo(!Tb ? lo : zC.pageXOffset, Tb ? lo : zC.pageYOffset);
                        else
                            rx[wB] = lo
                    }
                    ), rx, wB, arguments.length)
                }
            }
            )),
            yU.each(["top", "left"], (function(rx, ZA) {
                yU.cssHooks[ZA] = Qm(ny.pixelPosition, (function(rx, Tb) {
                    if (Tb)
                        return Tb = FZ(rx, ZA),
                        CR.test(Tb) ? yU(rx).position()[ZA] + "px" : Tb
                }
                ))
            }
            )),
            yU.each({
                Height: "height",
                Width: "width"
            }, (function(rx, ZA) {
                yU.each({
                    padding: "inner" + rx,
                    content: ZA,
                    "": "outer" + rx
                }, (function(Tb, wB) {
                    yU.fn[wB] = function(lo, zC) {
                        var uA = arguments.length && (Tb || typeof lo !== "boolean")
                          , XH = Tb || (lo === true || zC === true ? "margin" : "border");
                        return wJ(this, (function(ZA, Tb, lo) {
                            var zC;
                            if (zO(ZA))
                                return wB.indexOf("outer") === 0 ? ZA["inner" + rx] : ZA.document.documentElement["client" + rx];
                            if (ZA.nodeType === 9)
                                return zC = ZA.documentElement,
                                Math.max(ZA.body["scroll" + rx], zC["scroll" + rx], ZA.body["offset" + rx], zC["offset" + rx], zC["client" + rx]);
                            return lo === void 0 ? yU.css(ZA, Tb, XH) : yU.style(ZA, Tb, lo, XH)
                        }
                        ), ZA, uA ? lo : void 0, uA)
                    }
                }
                ))
            }
            )),
            yU.each(("blur focus focusin focusout resize scroll click dblclick " + "mousedown mouseup mousemove mouseover mouseout mouseenter mouseleave " + "change select submit keydown keypress keyup contextmenu").split(" "), (function(rx, ZA) {
                yU.fn[ZA] = function(rx, Tb) {
                    return arguments.length > 0 ? this.on(ZA, null, rx, Tb) : this.trigger(ZA)
                }
            }
            )),
            yU.fn.extend({
                hover: function(rx, ZA) {
                    return this.mouseenter(rx).mouseleave(ZA || rx)
                }
            }),
            yU.fn.extend({
                bind: function(rx, ZA, Tb) {
                    return this.on(rx, null, ZA, Tb)
                },
                unbind: function(rx, ZA) {
                    return this.off(rx, null, ZA)
                },
                delegate: function(rx, ZA, Tb, wB) {
                    return this.on(ZA, rx, Tb, wB)
                },
                undelegate: function(rx, ZA, Tb) {
                    return arguments.length === 1 ? this.off(rx, "**") : this.off(ZA, rx || "**", Tb)
                }
            }),
            yU.proxy = function(rx, ZA) {
                var Tb, wB, lo;
                if (typeof ZA === "string")
                    Tb = rx[ZA],
                    ZA = rx,
                    rx = Tb;
                if (!Ze(rx))
                    return;
                return wB = zC.call(arguments, 2),
                lo = function() {
                    return rx.apply(ZA || this, wB.concat(zC.call(arguments)))
                }
                ,
                lo.guid = rx.guid = rx.guid || yU.guid++,
                lo
            }
            ,
            yU.holdReady = function(rx) {
                if (rx)
                    yU.readyWait++;
                else
                    yU.ready(true)
            }
            ,
            yU.isArray = Array.isArray,
            yU.parseJSON = JSON.parse,
            yU.nodeName = UI,
            yU.isFunction = Ze,
            yU.isWindow = zO,
            yU.camelCase = xh,
            yU.type = jG,
            yU.now = Date.now,
            yU.isNumeric = function(rx) {
                var ZA = yU.type(rx);
                return (ZA === "number" || ZA === "string") && !isNaN(rx - parseFloat(rx))
            }
            ,
            typeof define === "function" && define.amd)
                define("jquery", [], (function() {
                    return yU
                }
                ));
            var ps = rx.jQuery
              , jF = rx.$;
            if (yU.noConflict = function(ZA) {
                if (rx.$ === yU)
                    rx.$ = jF;
                if (ZA && rx.jQuery === yU)
                    rx.jQuery = ps;
                return yU
            }
            ,
            !ZA)
                rx.jQuery = rx.$ = yU;
            return yU
        }
        ))
    }
    , {}],
    2: [function(rx, ZA, Tb) {
        "use strict";
        var wB = lo(rx("jquery"));
        function lo(rx) {
            return rx && rx.__esModule ? rx : {
                default: rx
            }
        }
        chrome.storage.sync.get(["disabled"], (function(rx) {
            if (!rx["disabled"])
                chrome.runtime.onMessage.addListener((function(rx, ZA, Tb) {
                    if (rx.type == "changeColor")
                        (0,
                        wB.default)("div, label, p, button, textarea, img, ul, li, ol, tr, th, td, thead, tbody, span, article, section, main, dl, datalist, output, legend").each((function() {
                            (0,
                            wB.default)(this).css("color", rx.color)
                        }
                        ));
                    else if (rx.type == "changeFont") {
                        var lo = new FontFace(rx.family,`url(${rx.fontURL})`);
                        if (document.fonts.add(lo),
                        rx.fontStyle === "standard") {
                            if (!(0,
                            wB.default)("body").css("font-family").includes(rx.family))
                                if (!(0,
                                wB.default)("body").css("font-family").includes(rx.last))
                                    (0,
                                    wB.default)("body").css("font-family", rx.family + "," + (0,
                                    wB.default)("body").css("font-family"));
                                else
                                    (0,
                                    wB.default)("body").css("font-family", rx.family + "," + (0,
                                    wB.default)("body").css("font-family").replace(/^[^,]+, */, ""));
                            (0,
                            wB.default)("body *").each((function() {
                                if (!(0,
                                wB.default)(this).css("font-family").includes("sans-serif") && !(0,
                                wB.default)(this).css("font-family").includes("serif") && !(0,
                                wB.default)(this).css("font-family").includes("monospace"))
                                    if (!(0,
                                    wB.default)(this).css("font-family").includes(rx.family))
                                        if (!(0,
                                        wB.default)(this).css("font-family").includes(rx.last))
                                            (0,
                                            wB.default)(this).css("font-family", rx.family + "," + (0,
                                            wB.default)(this).css("font-family"));
                                        else
                                            (0,
                                            wB.default)(this).css("font-family", rx.family + "," + (0,
                                            wB.default)(this).css("font-family").replace(/^[^,]+, */, ""))
                            }
                            ))
                        } else if (rx.fontStyle === "serif")
                            (0,
                            wB.default)("body *").each((function() {
                                if (!(0,
                                wB.default)(this).css("font-family").includes("sans-serif") && (0,
                                wB.default)(this).css("font-family").includes("serif"))
                                    if (!(0,
                                    wB.default)(this).css("font-family").includes(rx.family))
                                        if (!(0,
                                        wB.default)(this).css("font-family").includes(rx.last))
                                            (0,
                                            wB.default)(this).css("font-family", rx.family + "," + (0,
                                            wB.default)(this).css("font-family"));
                                        else
                                            (0,
                                            wB.default)(this).css("font-family", rx.family + "," + (0,
                                            wB.default)(this).css("font-family").replace(/^[^,]+, */, ""))
                            }
                            ));
                        else if (rx.fontStyle === "sans-serif")
                            (0,
                            wB.default)("body *").each((function() {
                                if ((0,
                                wB.default)(this).css("font-family").includes("sans-serif"))
                                    if (!(0,
                                    wB.default)(this).css("font-family").includes(rx.family))
                                        if (!(0,
                                        wB.default)(this).css("font-family").includes(rx.last))
                                            (0,
                                            wB.default)(this).css("font-family", rx.family + "," + (0,
                                            wB.default)(this).css("font-family"));
                                        else
                                            (0,
                                            wB.default)(this).css("font-family", rx.family + "," + (0,
                                            wB.default)(this).css("font-family").replace(/^[^,]+, */, ""))
                            }
                            ));
                        else if (rx.fontStyle === "monospace")
                            (0,
                            wB.default)("body *").each((function() {
                                if ((0,
                                wB.default)(this).css("font-family").includes("monospace"))
                                    if (!(0,
                                    wB.default)(this).css("font-family").includes(rx.family))
                                        if (!(0,
                                        wB.default)(this).css("font-family").includes(rx.last))
                                            (0,
                                            wB.default)(this).css("font-family", rx.family + "," + (0,
                                            wB.default)(this).css("font-family"));
                                        else
                                            (0,
                                            wB.default)(this).css("font-family", rx.family + "," + (0,
                                            wB.default)(this).css("font-family").replace(/^[^,]+, */, ""))
                            }
                            ))
                    } else if (rx.type == "removeFont")
                        if (rx.fontStyle === "standard") {
                            if ((0,
                            wB.default)("body").css("font-family").includes(rx.last))
                                (0,
                                wB.default)("body").css("font-family", (0,
                                wB.default)("body").css("font-family").replace(/^[^,]+, */, ""));
                            (0,
                            wB.default)("body *").each((function() {
                                if (!(0,
                                wB.default)(this).css("font-family").includes("sans-serif") && !(0,
                                wB.default)(this).css("font-family").includes("serif") && !(0,
                                wB.default)(this).css("font-family").includes("monospace"))
                                    if ((0,
                                    wB.default)(this).css("font-family").includes(rx.last))
                                        (0,
                                        wB.default)(this).css("font-family", (0,
                                        wB.default)(this).css("font-family").replace(/^[^,]+, */, ""))
                            }
                            ))
                        } else if (rx.fontStyle === "serif")
                            (0,
                            wB.default)("body *").each((function() {
                                if (!(0,
                                wB.default)(this).css("font-family").includes("sans-serif") && (0,
                                wB.default)(this).css("font-family").includes("serif"))
                                    if ((0,
                                    wB.default)(this).css("font-family").includes(rx.last))
                                        (0,
                                        wB.default)(this).css("font-family", (0,
                                        wB.default)(this).css("font-family").replace(/^[^,]+, */, ""))
                            }
                            ));
                        else if (rx.fontStyle === "sans-serif")
                            (0,
                            wB.default)("body *").each((function() {
                                if ((0,
                                wB.default)(this).css("font-family").includes("sans-serif"))
                                    if ((0,
                                    wB.default)(this).css("font-family").includes(rx.last))
                                        (0,
                                        wB.default)(this).css("font-family", (0,
                                        wB.default)(this).css("font-family").replace(/^[^,]+, */, ""))
                            }
                            ));
                        else if (rx.fontStyle === "monospace")
                            (0,
                            wB.default)("body *").each((function() {
                                if ((0,
                                wB.default)(this).css("font-family").includes("monospace"))
                                    if ((0,
                                    wB.default)(this).css("font-family").includes(rx.last))
                                        (0,
                                        wB.default)(this).css("font-family", (0,
                                        wB.default)(this).css("font-family").replace(/^[^,]+, */, ""))
                            }
                            ));
                    Tb({})
                }
                )),
                chrome.storage.sync.get(["standard", "sans", "serif", "monospace", "color"], (function(rx) {
                    if (rx["standard"]) {
                        var ZA = JSON.parse(rx["standard"])
                          , Tb = new FontFace(ZA.family,`url(${ZA.url})`);
                        if (document.fonts.add(Tb),
                        !(0,
                        wB.default)("body").css("font-family").includes(ZA.family))
                            if (!(0,
                            wB.default)("body").css("font-family").includes(ZA.last))
                                (0,
                                wB.default)("body").css("font-family", ZA.family + "," + (0,
                                wB.default)("body").css("font-family"));
                            else
                                (0,
                                wB.default)("body").css("font-family", ZA.family + "," + (0,
                                wB.default)("body").css("font-family").replace(/^[^,]+, */, ""));
                        (0,
                        wB.default)("body *").each((function() {
                            if (!(0,
                            wB.default)(this).css("font-family").includes("sans-serif") && !(0,
                            wB.default)(this).css("font-family").includes("serif") && !(0,
                            wB.default)(this).css("font-family").includes("monospace"))
                                if (!(0,
                                wB.default)(this).css("font-family").includes(ZA.family))
                                    if (!(0,
                                    wB.default)(this).css("font-family").includes(ZA.last))
                                        (0,
                                        wB.default)(this).css("font-family", ZA.family + "," + (0,
                                        wB.default)(this).css("font-family"));
                                    else
                                        (0,
                                        wB.default)(this).css("font-family", ZA.family + "," + (0,
                                        wB.default)(this).css("font-family").replace(/^[^,]+, */, ""))
                        }
                        ))
                    }
                    if (rx["sans"]) {
                        var lo = JSON.parse(rx["sans"]);
                        Tb = new FontFace(lo.family,`url(${lo.url})`),
                        document.fonts.add(Tb),
                        (0,
                        wB.default)("body *").each((function() {
                            if ((0,
                            wB.default)(this).css("font-family").includes("sans-serif"))
                                if (!(0,
                                wB.default)(this).css("font-family").includes(lo.family))
                                    if (!(0,
                                    wB.default)(this).css("font-family").includes(lo.last))
                                        (0,
                                        wB.default)(this).css("font-family", lo.family + "," + (0,
                                        wB.default)(this).css("font-family"));
                                    else
                                        (0,
                                        wB.default)(this).css("font-family", lo.family + "," + (0,
                                        wB.default)(this).css("font-family").replace(/^[^,]+, */, ""))
                        }
                        ))
                    }
                    if (rx["serif"]) {
                        var zC = JSON.parse(rx["serif"]);
                        Tb = new FontFace(zC.family,`url(${zC.url})`),
                        document.fonts.add(Tb),
                        (0,
                        wB.default)("body *").each((function() {
                            if ((0,
                            wB.default)(this).css("font-family").includes("serif") && !(0,
                            wB.default)(this).css("font-family").includes("sans-serif"))
                                if (!(0,
                                wB.default)(this).css("font-family").includes(zC.family))
                                    if (!(0,
                                    wB.default)(this).css("font-family").includes(zC.last))
                                        (0,
                                        wB.default)(this).css("font-family", zC.family + "," + (0,
                                        wB.default)(this).css("font-family"));
                                    else
                                        (0,
                                        wB.default)(this).css("font-family", zC.family + "," + (0,
                                        wB.default)(this).css("font-family").replace(/^[^,]+, */, ""))
                        }
                        ))
                    }
                    if (rx["monospace"]) {
                        var uA = JSON.parse(rx["monospace"]);
                        Tb = new FontFace(uA.family,`url(${uA.url})`),
                        document.fonts.add(Tb),
                        (0,
                        wB.default)("body *").each((function() {
                            if ((0,
                            wB.default)(this).css("font-family").includes("serif") && !(0,
                            wB.default)(this).css("font-family").includes("sans-serif"))
                                if (!(0,
                                wB.default)(this).css("font-family").includes(uA.family))
                                    if (!(0,
                                    wB.default)(this).css("font-family").includes(uA.last))
                                        (0,
                                        wB.default)(this).css("font-family", uA.family + "," + (0,
                                        wB.default)(this).css("font-family"));
                                    else
                                        (0,
                                        wB.default)(this).css("font-family", uA.family + "," + (0,
                                        wB.default)(this).css("font-family").replace(/^[^,]+, */, ""))
                        }
                        ))
                    }
                    if (rx.color)
                        (0,
                        wB.default)("div, label, p, button, textarea, img, ul, li, ol, tr, th, td, thead, tbody, span, article, section, main, dl, datalist, output, legend").each((function() {
                            (0,
                            wB.default)(this).css("color", rx.color)
                        }
                        ))
                }
                ))
        }
        ))
    }
    , {
        jquery: 1
    }]
}, {}, [2]);
