local function fibonacci(value)
    if value < 2 then
        return value
    end
    return fibonacci(value - 1) + fibonacci(value - 2)
end

local iterations = tonumber(arg[1]) or 1
local result = 0
for _ = 1, iterations do
    result = fibonacci(20)
end
print(result)
